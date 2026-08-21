//! Client-side entity interpolation: turning the 20 Hz stream of
//! [`EntityView`](lodestone_client::EntityView) snapshots into smooth per-frame
//! render transforms.
//!
//! Since Stage 1 of [`docs/bevy-migration.md`](../../../docs/bevy-migration.md)
//! the per-entity render state lives in **`bevy_ecs` components** — one entity
//! per tracked mob, carrying [`InterpFrom`] / [`InterpTo`] / [`InterpClock`] /
//! [`WalkAnim`] / [`ItemPhysics`] — and the work is done by systems registered
//! into the schedules `lodestone-ecs` owns:
//!
//! | system | schedule / set |
//! |---|---|
//! | [`advance_interp_clocks`] | [`Update`] / `FrameSet::Interpolate` |
//! | [`tick_item_physics`] | [`GameTick`] / `TickSet::Physics` |
//! | [`tick_walk_animation`] | [`GameTick`] / `TickSet::Animate` |
//! | [`tick_pickup_animations`] | [`GameTick`] / `TickSet::Animate` |
//! | [`extract_entity_draws`] | [`Extract`] / `ExtractSet::Entities` |
//! | [`extract_pickup_draws`] | [`Extract`] / `ExtractSet::Entities`, after the above |
//!
//! [`EntityInterpolator`] is the driver for those schedules and nothing else: it
//! owns the `World`, runs the schedules in order, and hands out the extracted
//! [`EntityDraw`] list. One piece of the fold is still deliberately **not** a
//! system — [`fold_entities`], which is called by hand from `sim.rs` rather
//! than scheduled; read its own docs before moving it.
//! [`tick_item_physics`] used to be blocked on the same `'static`-resource
//! problem, until [`lodestone_ecs::player::CollisionSource`] gave the
//! collision borrow somewhere `'static` to live — see that system's docs.
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
//! Sampling the drawn position once per 20 Hz tick measures `v` instead, because
//! that is what vanilla is measuring: on the client the entity's own position has
//! already been advanced by `InterpolationHandler`, so `getX() - xo` is the
//! *interpolated* step, not the packet delta. The two agree under dense packets
//! and under sparse ones, which the gap measure never does. That sampling is
//! [`tick_walk_animation`], and it runs on a fixed 20 Hz clock rather than per
//! frame, because `WalkAnimationState` is a tick-rate state machine and driving
//! it per frame would make swing speed depend on frame rate.
//!
//! This module is deliberately GPU-free, so the interpolation is unit-testable
//! without a device or a server: [`fold_entities`] reads
//! [`lodestone_ecs::entity`]'s ingest components straight out of the shared
//! `World` (via [`resolve_entity_facts`]) and folds them into the render
//! component set above. The output is a flat list of [`EntityDraw`]s — type
//! path, feet position, body yaw and scale — that the renderer resolves into
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
//! does the same for entities whose [`RenderKind`] is
//! [`ITEM_ENTITY_TYPE_PATH`]: an entity carrying an [`ItemPhysics`] component
//! runs [`step_item_physics`] (gravity `0.04`, air drag `0.98` —
//! [`lodestone_entity::item_entity`]'s vanilla constants, not reimplemented)
//! once per real 20 Hz tick, and the render ease ([`InterpClock::t`] /
//! [`INTERP_WINDOW`]) is re-anchored off *that* simulated position each tick
//! rather than off the sparse network packet. A server correction (when one
//! arrives) resets the simulated position/velocity to the authoritative value
//! rather than fighting it. While the last-known snapshot reports the item at
//! rest on the ground, the simulation is paused rather than resimulated
//! needlessly — see [`EntityFacts::on_ground`].
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
//! world to query and keeps the old free-fall behaviour, but the real path —
//! the [`ItemCollision`] resource [`crate::sim::Sim`] inserts each tick, or
//! [`EntityInterpolator::update_with_view`] for a harness — resolves real
//! collision every tick. This is bounded by
//! the `view`'s own coverage (the live path's is the loaded-chunk radius
//! around the player), not global: a drop far outside that radius still
//! free-falls until it is back in range, same as before this existed.
//!
//! Every other entity type is unaffected: it carries no [`ItemPhysics`]
//! component at all and the original pure position ease runs exactly as before.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy_ecs::prelude::{
    Commands, Component, Entity, IntoScheduleConfigs, Query, Res, ResMut, Resource, With, Without,
};
use bevy_ecs::world::World;
use glam::Vec3;
use lodestone_assets::ResourceLocation;
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::entity::{
    AttackSwing, DeathTime, EntityFlags, EntityIndex, ExperienceOrbValue, FallingBlockState,
    HurtTime, ItemFrameRotation, ItemUse, MinecraftEntityId, MobState, OnGround, Pose,
};
use lodestone_ecs::player::{
    CollisionSource, LocalPlayer, PhysicsState, PlayerCollision, Profile,
};
use lodestone_ecs::{CorePlugin, Extract, ExtractSet, FrameSet, GameTick, TickSet, Update};
use lodestone_entity::item_entity::{ITEM_AIR_DRAG, ITEM_GRAVITY, ItemMotion};
use lodestone_entity::pose::{
    ADULT_LIMB_SCALE, BABY_LIMB_SCALE, LIMB_SWING_SMOOTHING, MAX_HEAD_YAW, WalkAnimation,
    clamp_head_to_body, walk_target_speed,
};
use lodestone_model::event::{EntityVariant, EquipmentSlot, Reported};
use lodestone_model::Text;
use lodestone_physics::{
    CollisionView, EntityDimensions, EntityMotion, MoveContext, PhysicsProfile, Vec3d, mth,
    move_entity,
};
use lodestone_render::{AnimInput, ArmPose, mob_draws_bow_when_aggressive, renderer_is_avatar};

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
///
/// **This stays a plain function, called by a system rather than being one**,
/// for the same reason `docs/bevy-migration.md` §8 keeps `lodestone-physics` a
/// library: it is the vanilla-constant carrier, and its per-tick trace is what
/// the tests below pin.
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
/// this: since §4.1(c) `crate::sim::Sim` inserts an [`ItemCollision`] resource
/// built from the player's loaded chunks before each `GameTick` run.
#[derive(Debug)]
struct OpenAir;

impl CollisionView for OpenAir {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<lodestone_physics::Aabb>) {}
}

/// [`OpenAir`] as a [`CollisionSource`]: it owns nothing, so the borrow
/// [`CollisionSource::with_view`] hands out is trivially satisfied by lending
/// back the same zero-sized value.
impl CollisionSource for OpenAir {
    fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
        f(self);
    }
}

/// Converts an [`ItemMotion`]-space `f64` [`lodestone_model::Vec3`] back into
/// render-space [`glam::Vec3`].
fn to_glam_vec3(v: lodestone_model::Vec3) -> Vec3 {
    Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

/// One physics tick, in seconds.
///
/// Narrowed from [`lodestone_ecs::TICK_PERIOD`] rather than written as `0.05`
/// again: the render eases below are all `f32`, but the *authoritative* period is
/// the `f64` one the single accumulator counts in, and two spellings of "a tick"
/// is how the pre-§4.1(c) clocks came to differ by 1.5e-8 per tick on top of the
/// clamp that actually mattered.
const TICK: f32 = lodestone_ecs::TICK_PERIOD as f32;

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
/// Server ticks per second, for the continuous `ageInTicks` clock.
const TICKS_PER_SECOND: f32 = 20.0;

const POS_EPS: f32 = 1.0e-4;

/// Yaw change (degrees) below which a snapshot is treated as "no turn". Applies
/// to body yaw, head yaw and pitch alike.
const YAW_EPS: f32 = 1.0e-2;

/// A resolved nametag (issue #100): the plain text to draw above the entity,
/// plus whether the depth-see-through pass applies.
///
/// Resolved once, inside [`resolve_entity_facts`] — a player's tag from the
/// tab list, every other entity's from its `CUSTOM_NAME`/
/// `CUSTOM_NAME_VISIBLE` metadata — so [`EntityFacts`], [`EntityDraw`] and
/// the nametag pass never need to know the two rules differ. See
/// [`resolve_entity_facts`]'s doc for the exact vanilla predicates (jar
/// file:line) and `docs/entity-nametags.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameTag {
    /// The styled component tree to draw. `gpu/nametag.rs`'s
    /// `push_entity_quads` calls [`lodestone_model::Text::to_spans`] on this
    /// directly — colour (including a hex [`lodestone_model::text::TextColor::Rgb`],
    /// which a `to_legacy_string`/`from_legacy` round trip could never carry,
    /// since legacy `§` codes have no hex form), bold, italic, underline and
    /// strikethrough all survive intact. A translation-key custom name, the
    /// rare case, draws its raw key rather than resolving through the
    /// shell's chat language table; see `docs/entity-nametags.md`.
    pub text: Text,
    /// Whether the depth-testless, faded pass draws in addition to the normal
    /// depth-tested one — `false` while the entity is sneaking
    /// (`Entity.isDiscrete()`), which is when vanilla suppresses it. See
    /// `gpu/nametag.rs`'s module doc for the two passes' exact depth
    /// settings.
    pub see_through: bool,
}

/// The ingest-side facts [`fold_entities`] needs for one server entity id, as
/// of this frame — resolved fresh each fold by [`resolve_entity_facts`], never
/// stored.
///
/// # Replaces `EntitySnapshot` (issue #36)
///
/// This module used to receive a `Vec<EntitySnapshot>` built by
/// `net::entity_snapshot` from a *separate, already-released* read of the
/// client's entity table (`NetClient::entity_snapshots`), because that read and
/// this fold used to look like two different `World`s. They are not: since
/// §4.1(c) (`docs/entity-components.md`), `lodestone_ecs::ingest`'s components
/// and this module's render components live in the **one** `World`
/// `crate::sim::Sim` owns, so resolving to an owned `Vec` before taking the
/// write guard here was a redundant round trip through a public type — read
/// `docs/entity-components.md`'s "Update, and it changes the plan" for why the
/// schedule reorder that issue's title implies is a separate, larger change
/// this deletion does **not** need. `EntityFacts` is private and exists only
/// as this fold's own scratch space: nothing outside this module ever sees or
/// builds one, which is the whole point.
#[derive(Debug, Clone, PartialEq)]
struct EntityFacts {
    /// The server-assigned entity id (interpolation key).
    id: i32,
    /// The entity type's canonical path (e.g. `"pig"`), for model resolution.
    type_path: String,
    /// Feet position in world space.
    feet: Vec3,
    /// Body yaw in degrees.
    yaw: f32,
    /// Head yaw in degrees (absolute). Tracked separately from the body: a
    /// walking mob keeps its body facing its movement while its head turns to
    /// track a target, so this is never derived from `yaw`.
    head_yaw: f32,
    /// Head pitch in degrees (look up/down).
    pitch: f32,
    /// Uniform render scale (baby mobs are drawn smaller), derived from
    /// [`lodestone_ecs::entity::Baby`] in [`resolve_entity_facts`].
    scale: f32,
    /// Which item a dropped item (or other item-displaying entity) is showing.
    ///
    /// Exactly the shape [`lodestone_ecs::entity::DisplayItem`]'s own field is:
    /// [`Reported::Unreported`] is "the server has never reported a stack for
    /// this entity", [`Reported::Reported(None)`](Reported::Reported) is an
    /// explicitly *empty* stack. `Unreported` therefore means "unknown", and
    /// [`fold_entities`] leaves any previously recorded stack alone rather than
    /// clearing it — a drop names itself once and then goes quiet, so treating
    /// silence as "empty" would blank it a frame later.
    ///
    /// This is a [`ResourceLocation`], not a model `ItemStack`: the stack's
    /// `count` and data components are narrowed away in
    /// [`resolve_entity_facts`] and carried separately as sibling facts —
    /// [`Self::count`], [`Self::foil`], [`Self::item_dyed_color`] and
    /// [`Self::item_potion_color`] — the same additive pattern
    /// [`Self::equipment_dye`] uses for equipment.
    item: Reported<ResourceLocation>,
    /// The entity's last-reported velocity in blocks per tick
    /// (`set_entity_motion`/`add_entity`), when the server has ever sent one.
    ///
    /// This is what the [`ItemPhysics`] component seeds and re-anchors its
    /// ballistic simulation from — see the module docs on why a dropped item
    /// needs real physics rather than a position ease. `None` is "never
    /// reported", not "zero"; a zero velocity is reported as `Some(Vec3::ZERO)`.
    velocity: Option<Vec3>,
    /// Whether the server last reported this entity resting on the ground
    /// (`on_ground` on `add_entity`/`teleport_entity`/`move_entity`).
    ///
    /// [`tick_item_physics`] pauses its simulation while this is `true`, because
    /// a resting item does not need resimulating.
    on_ground: bool,
    /// What this entity is wearing and holding, keyed by slot, as
    /// `SET_EQUIPMENT` last reported it.
    ///
    /// The inner `Option` is the *slot's* nesting, not the field's: a slot
    /// **absent** from this list is "the server has never mentioned it", while a
    /// slot present with `None` is an explicit "this slot is empty". That is
    /// [`lodestone_ecs::entity::Equipment`]'s contract preserved verbatim, and
    /// it is why this is a list of pairs rather than a fixed-size array of
    /// `Option`s.
    ///
    /// The whole list is *accumulated server-side of this type*
    /// (`lodestone_ecs::ingest`'s `apply_entity_equipment` merges each update
    /// into the `Equipment` component and never clears), so every fold
    /// carries the complete current set and [`fold_entities`] replaces its
    /// record wholesale — unlike [`Self::item`], which arrives once and must
    /// never be cleared by silence.
    ///
    /// Armour slots reach a pixel too — `RenderState`'s `prepare_armour` walks
    /// `ArmourSlot::ALL` against this same list.
    equipment: Vec<(EquipmentSlot, Option<ResourceLocation>)>,
    /// Per-slot `minecraft:dyed_color`, alongside [`Self::equipment`] rather
    /// than folded into it — see `docs/armour-rendering.md`'s "hop 2" for why
    /// this is additive rather than a wider tuple: [`Self::equipment`]'s shape
    /// is depended on by several call sites in this module and in `gpu.rs`,
    /// none of which need to change just because a dye is now readable.
    ///
    /// A slot **absent** here means "no dye reported for this slot" (either
    /// the slot holds nothing, or it holds an item with no
    /// `minecraft:dyed_color` patch) — `lodestone_render::entity::
    /// armour_layer_tint_with_dye` already treats a missing dye and a
    /// zero-valued one identically (`dyed_color_zero_reads_as_undyed`), so
    /// there is no information lost by not distinguishing "never reported"
    /// from "reported, and it was zero" the way [`Self::equipment`] does for
    /// item identity.
    equipment_dye: Vec<(EquipmentSlot, u32)>,
    /// Per-slot `minecraft:trim` (issue #17), narrowed exactly as
    /// [`Self::equipment_dye`] is and additive for the same reason.
    ///
    /// Trim is a *texture* rather than a tint, so unlike dye it cannot ride an
    /// instance row — it forces its own batch. That is a renderer concern; here it
    /// is just one more per-slot fact off the same `ItemStack`.
    equipment_trim: Vec<(EquipmentSlot, lodestone_model::item::ArmorTrim)>,
    /// The entity's decoded cosmetic variant (sheep dye/shear, villager type,
    /// horse markings, …), as last reported.
    ///
    /// Exactly [`lodestone_ecs::entity::Variant`]'s own contract, copied
    /// through verbatim like [`Self::equipment`]: `None` means the server has
    /// never reported an override, which is a different state from a
    /// known-but-default variant. There is no "explicitly cleared" state to
    /// preserve here — vanilla never un-reports a variant — so unlike
    /// [`Self::item`] this needs no [`Reported`] wrapper.
    ///
    /// Only [`EntityVariant::Dyed`] reaches a pixel today, and only when
    /// [`Self::type_path`] is `"sheep"` — see [`EntityDraw::wool`] and
    /// `docs/entity-rendering.md`'s "Render layers: sheep wool" section.
    variant: Option<EntityVariant>,
    /// How many items the stack named by [`Self::item`] represents, as last
    /// reported. Meaningless when [`Self::item`] is [`Reported::Unreported`]
    /// or an explicit empty stack; `1` in both of those cases, and whenever
    /// the server has never reported a stack for this entity at all.
    ///
    /// Narrowed from `DisplayItem`'s full `ItemStack::count` in
    /// [`resolve_entity_facts`]; unlike the data components dropped there, it
    /// changes *how many* copies vanilla draws rather than how one looks
    /// (`ItemClusterRenderState::getRenderedAmount`: 1 copy at count ≤ 1, then
    /// 2, 3, 4, 5 as the count passes 1, 16, 32 and 48).
    count: u32,
    /// Whether the carried stack has the enchantment foil — `ItemStack.hasFoil`,
    /// narrowed from `DisplayItem`'s components the same way [`Self::count`] is.
    foil: bool,
    /// The stack named by [`Self::item`]'s `minecraft:dyed_color`, narrowed from
    /// `DisplayItem`'s components the same way [`Self::count`] is — additive
    /// alongside `item` rather than folded into it, mirroring
    /// [`Self::equipment_dye`]'s own reason. `None` for an undyed stack, or
    /// whenever [`Self::item`] carries no stack.
    ///
    /// Without this a dropped dyed-leather item, and a thrown lingering/splash
    /// potion — which reaches the world through this same field, since a
    /// projectile's stack rides the identical `DATA_ITEM_STACK` sync a dropped
    /// item uses — drew the item definition's plain default colour rather than
    /// the real one.
    item_dyed_color: Option<u32>,
    /// The stack's already-mixed `minecraft:potion_contents` colour, mirroring
    /// [`Self::item_dyed_color`] exactly — see
    /// [`lodestone_model::item::ItemComponents::potion_color`]'s doc for why
    /// this is the pre-mixed colour and not the raw patch.
    item_potion_color: Option<u32>,
    /// This entity's resolved nametag (issue #100), or `None` when nothing
    /// should draw above it — a mob with no visible custom name, or a player
    /// entity with no matching tab-list entry. See [`NameTag`].
    name_tag: Option<NameTag>,
    /// A creeper's synced fuse direction (`Creeper.DATA_SWELL_DIR`), as last
    /// reported — meaningless when [`Self::type_path`] is not `"creeper"`.
    ///
    /// `None` means "the server has never reported this", exactly
    /// [`Self::variant`]'s own contract: [`spawn_track`] seeds a fresh
    /// creeper's [`CreeperFuse`] from vanilla's own idle default
    /// ([`CreeperFuse::IDLE`]) rather than treating `None` as zero, and
    /// [`update_track`] only overwrites the direction when a fold actually
    /// carries one — silence must not reset a mid-fuse creeper back to idle.
    creeper_swell_dir: Option<i32>,
    /// The skin this player declares, from the tab-list profile's `textures`
    /// property — `None` for every non-player and for every player whose
    /// profile declares none.
    ///
    /// **`None` is the normal case against every one of our own oracles**: an
    /// offline-mode server derives the account UUID from the username and sends
    /// no `textures` property at all. It resolves to the default sheet on the
    /// wide rig, which is what a remote player looked like before this existed.
    ///
    /// Carried as a whole [`crate::remote_skins::RemoteSkin`] rather than a bare
    /// URL because the rig and the sheet have to change **together** — see that
    /// type's doc.
    player_skin: Option<crate::remote_skins::RemoteSkin>,
}

/// A sheep's decoded wool state, narrowed from [`EntityFacts::variant`] for
/// [`EntityDraw::wool`].
///
/// Kept as its own small type rather than passing [`EntityVariant`] straight
/// through so a consumer needs no `match` on variant arms that can never apply
/// to a sheep (`Villager`, `Horse`, `Keyed`) — [`sheep_wool`] is the one place
/// that does that matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SheepWool {
    /// Dye/wool colour ordinal, `0..=15` — the same value
    /// `lodestone_assets::entity_models::sheep_wool_tint` indexes.
    pub color: u8,
    /// Whether the sheep has been sheared. **Not** filtered out here: a
    /// sheared sheep still yields `Some(SheepWool { sheared: true, .. })`
    /// rather than `None`, so the data stays honest about what the server
    /// actually reported. Vanilla's own "sheared sheep grow no wool mesh" gate
    /// belongs at the point that draws the mesh
    /// (`RenderState::prepare_wool`, see `docs/entity-rendering.md`), the same
    /// way `EntityDraw::equipment` keeps armour slots it cannot yet draw
    /// rather than pre-filtering them.
    pub sheared: bool,
}

/// Narrows a snapshot's decoded variant to the sheep-wool payload
/// [`EntityDraw::wool`] carries.
///
/// Gated on the **resolved type path being exactly `"sheep"`**, never on
/// `AnimFamily::Quadruped` (shared by pig, cow and wolf) — the same pig/cow
/// trap `docs/entity-rendering.md` documents for the armour attach applies
/// here, worse, because wool has no gate at all inside the mesh geometry
/// itself the way a humanoid check does.
#[must_use]
fn sheep_wool(type_path: &str, variant: Option<&EntityVariant>) -> Option<SheepWool> {
    if type_path != "sheep" {
        return None;
    }
    match variant {
        Some(EntityVariant::Dyed { color, sheared }) => Some(SheepWool {
            color: *color,
            sheared: *sheared,
        }),
        _ => None,
    }
}

/// The entity-type path a **dropped item** reports (`minecraft:item`).
///
/// It has no [`entity_models`](lodestone_render::EntityModelSet) entry and never
/// will: an item entity is not a cuboid part rig, it is an *item model* drawn in
/// the world. `EntityModelSet::resolve` therefore skips it, which is why a drop
/// reaches [`EntityDraw`] but no pixels — the renderer picks these out by type
/// path and draws them through the model pipeline instead.
pub const ITEM_ENTITY_TYPE_PATH: &str = "item";

/// The entity-type path an **experience orb** reports (`minecraft:experience_orb`).
///
/// Like [`ITEM_ENTITY_TYPE_PATH`], it has no
/// [`entity_models`](lodestone_render::EntityModelSet) entry and never will:
/// `ExperienceOrbRenderer` is one camera-facing quad, not a cuboid part rig, so
/// `EntityModelSet::resolve` skips it and `RenderState::prepare_orbs` picks it out
/// by type path and draws it through the orb billboard pipeline instead.
pub const EXPERIENCE_ORB_TYPE_PATH: &str = "experience_orb";

/// A single entity ready to draw this frame: its model type and interpolated
/// transform inputs. The renderer turns this into an
/// [`EntityInstance`](lodestone_render::EntityInstance).
///
/// Produced by [`extract_entity_draws`], the `ExtractSet::Entities` system —
/// `docs/bevy-migration.md` §4.4's rule that extract systems live upstream of
/// `lodestone-render`, which stays bevy-free, and that they emit plain PODs it
/// already consumes.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityDraw {
    /// The server-assigned entity id. Carried through to the draw because a
    /// dropped item's bob/spin phase is derived from it
    /// ([`item_bob_offset`](lodestone_render::entity::item_bob_offset)) — vanilla rolls
    /// that phase from a client RNG we cannot observe, so a stable hash of the
    /// id stands in for it.
    pub id: i32,
    /// The entity type's canonical path (e.g. `"pig"`).
    ///
    /// `Arc<str>`, not `String` (issue #523) — cloned from [`RenderKind`] once
    /// per tracked entity per frame in `extract_entity_draws`; see that
    /// component's doc for why a refcount bump replaced a heap allocation
    /// here.
    pub type_path: Arc<str>,
    /// Which item's model to draw, for any entity whose server metadata
    /// carried an `ITEM_STACK` field — a dropped item
    /// ([`ITEM_ENTITY_TYPE_PATH`]), an item frame's contents
    /// (`ItemFrame.DATA_ITEM`, including a framed `filled_map`), and a thrown
    /// projectile's stack (`ThrowableItemProjectile`/`Fireball`/`EyeOfEnder`
    /// all sync through the same serializer). `None` for an entity that has
    /// never reported one.
    ///
    /// **This used to be narrowed to [`ITEM_ENTITY_TYPE_PATH`] in
    /// `extract_entity_draws`, which is what made the other three consumers
    /// dead**; the draw sites each gate on their own entity type, so the
    /// narrowing bought nothing and cost every framed item, every framed map
    /// and every projectile's live tint.
    pub item: Option<ResourceLocation>,
    /// What this entity is holding/wearing, narrowed to the slots that actually
    /// have something in them: an entry here means "there is an item in this
    /// slot", so the renderer needs no second `Option` check.
    ///
    /// **Stale note, kept for history:** this used to say only `MainHand`/
    /// `OffHand` could reach a pixel, because the `entity_models` corpus had
    /// no armour layer at all. `docs/armour-rendering.md` landed a *separate*
    /// humanoid mesh set since — `RenderState::prepare_armour` in `gpu.rs`
    /// now walks `ArmourSlot::ALL` against this same list, so every humanoid
    /// slot draws, not just the two hand slots.
    ///
    /// Order follows [`EquipmentSlot::ALL`] only by accident of what the server
    /// sent; treat it as an unordered set.
    pub equipment: Vec<(EquipmentSlot, ResourceLocation)>,
    /// Per-slot `minecraft:dyed_color`, mirroring
    /// [`EntityFacts::equipment_dye`] narrowed the same way `equipment`
    /// narrows [`EntityFacts::equipment`] — see that field's doc for why
    /// this is additive rather than folded into `equipment`'s own tuple.
    pub equipment_dye: Vec<(EquipmentSlot, u32)>,
    /// Per-slot `minecraft:trim` (issue #17), mirroring
    /// [`EntityFacts::equipment_trim`] and narrowed exactly as
    /// [`Self::equipment_dye`] is.
    ///
    /// Additive rather than folded into [`Self::equipment`]'s tuple for that
    /// field's reason, and additive rather than folded into `equipment_dye`'s
    /// because an item can carry both: trimmed leather armour is dyed *and*
    /// trimmed, and the two reach the GPU differently — dye as an instance tint,
    /// trim as its own texture and therefore its own batch.
    pub equipment_trim: Vec<(EquipmentSlot, lodestone_model::item::ArmorTrim)>,
    /// This entity's wool state, when [`Self::type_path`] is `"sheep"` and a
    /// variant has been reported — `None` for every other entity type
    /// unconditionally, per [`sheep_wool`]'s gate.
    ///
    /// **Not yet drawn.** The mesh/tint/pose plumbing
    /// (`WoolMesh`/`SheepWoolModelSet::attach` in
    /// `lodestone-render/src/entity.rs`, `RenderState::prepare_wool` in
    /// `gpu.rs`) is specified but not landed — see
    /// `docs/entity-rendering.md`'s "Render layers: sheep wool" section. This
    /// field is the last hop that was missing before that work; it does not
    /// draw anything by itself.
    pub wool: Option<SheepWool>,
    /// How many items [`Self::item`] represents, when it is `Some`.
    /// Meaningless (and left at the neutral `1`) for every entity with no
    /// reported stack, and for the consumers that draw one copy whatever the
    /// count — an item frame holds a stack and draws it once.
    ///
    /// `prepare_item_geometry` turns this into vanilla's 1–5 copies via
    /// `lodestone_render::entity::rendered_amount`, scattered by
    /// `item_cluster_jitter` — see `docs/dropped-items.md`.
    pub count: u32,
    /// Whether the carried stack is enchanted, so the drop gets the glint second
    /// pass. `false` for every entity with no reported stack. Read by the
    /// framed-item pass too, not only the drop pass.
    pub foil: bool,
    /// [`Self::item`]'s `minecraft:dyed_color`, mirroring
    /// [`EntityFacts::item_dyed_color`] narrowed the same way [`Self::count`]
    /// narrows `EntityFacts::count`. `None` for an undyed stack or when
    /// [`Self::item`] is `None`.
    ///
    /// Fed into [`lodestone_render::stamp_live_item_tint`] by
    /// `gpu::world_items`, alongside [`Self::item_potion_color`] — the pair
    /// that resolves a dropped item's or a thrown projectile's real tint
    /// instead of the item definition's plain default.
    pub item_dyed_color: Option<u32>,
    /// [`Self::item`]'s already-mixed `minecraft:potion_contents` colour,
    /// mirroring [`Self::item_dyed_color`] exactly — see
    /// [`lodestone_model::item::ItemComponents::potion_color`]'s doc for why
    /// this is the pre-mixed colour and not the raw patch.
    pub item_potion_color: Option<u32>,
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
    /// The block state this entity is imitating, when it is a
    /// `minecraft:falling_block` and its spawn packet has been decoded — the
    /// global block-state id, which is the key
    /// [`CrackResolver::state_quads`](lodestone_render::CrackResolver::state_quads)
    /// is indexed by.
    ///
    /// `None` for every other entity type, and the switch the moving-block-model
    /// pass keys on (`gpu/moving_blocks.rs`). Absence is deliberate rather than a
    /// sentinel `0`: state id `0` is a real state (`minecraft:air`), so a caller
    /// could not tell "not a falling block" from "a falling block made of air".
    ///
    /// Bridged off the *ingest* entity through [`EntityIndex`] in
    /// [`extract_entity_draws`], like [`Self::hurt`] and [`Self::item_use`], not
    /// folded through `EntityFacts` — see [`Self::hurt`] for why that hop is
    /// avoided.
    pub block_state: Option<u32>,
    /// Which of the eight 45° steps the stack in an item frame is turned to —
    /// `ItemFrame.getRotation()`, `0..8`.
    ///
    /// `0` for everything that is not an item frame, and for a frame whose
    /// rotation has not been reported: that is vanilla's own accessor default
    /// (an upright item), so unlike [`Self::block_state`] there is nothing an
    /// `Option` would distinguish. A frame *always* draws its contents; only
    /// their in-plane angle is at stake.
    ///
    /// Bridged off the *ingest* entity through [`EntityIndex`] in
    /// [`extract_entity_draws`], like [`Self::block_state`] above.
    pub item_frame_rotation: u8,
    /// This entity's resolved nametag (issue #100), narrowed from
    /// [`RenderNameTag`]. `None` draws nothing — the common case for every
    /// entity with no visible custom name.
    pub name_tag: Option<NameTag>,
    /// Whether the hurt/death **red overlay** applies to this entity's model
    /// this frame — vanilla's `state.hasRedOverlay = entity.hurtTime > 0 ||
    /// entity.deathTime > 0` (`LivingEntityRenderer.java`), issue #98.
    ///
    /// Boolean, not a fade: vanilla does not interpolate by how much of
    /// `hurtTime` remains, so neither does this (see
    /// [`lodestone_render::EntityInstanceRaw::with_hurt_overlay`]).
    ///
    /// Read off [`lodestone_ecs::entity::HurtTime`] through [`EntityIndex`] in
    /// [`extract_entity_draws`], **not** folded through [`EntityFacts`] —
    /// exactly like [`Self::anim`]'s `attack_anim`, and for the same reason:
    /// the component lives on the *ingest* entity rather than the render one.
    /// `docs/combat.md`'s original patch spec called for an
    /// `EntitySnapshot::hurt` field (`EntitySnapshot` was the boundary type
    /// issue #36 deleted); that would have added a third hop and rippled
    /// through ~15 struct literals for a value the extract system can already
    /// reach directly.
    ///
    /// Both halves of the disjunction are live: [`Self::death_time`] carries
    /// `deathTime`, so the overlay now persists through the fall-over instead of
    /// ending ten ticks after the killing blow.
    pub hurt: bool,
    /// This entity's `deathTime + partialTicks` while it is dying, `0.0` while it
    /// is alive — vanilla's `LivingEntityRenderer.extractRenderState` writes
    /// `state.deathTime = entity.deathTime > 0 ? entity.deathTime + partialTicks : 0.0F`.
    ///
    /// Read off [`lodestone_ecs::entity::DeathTime`] through [`EntityIndex`] in
    /// [`extract_entity_draws`], bridged exactly like [`Self::hurt`] and for the
    /// same reason — the component lives on the *ingest* entity.
    ///
    /// # One field, two consumers, and one of them is a rotation
    ///
    /// It feeds [`Self::hurt`]'s `deathTime > 0` half (right here, in this module)
    /// and the **fall-over rotation**
    /// ([`lodestone_render::entity_anim::death_fall_over_degrees`], composed into
    /// the placement by [`lodestone_render::dying_entity_model_matrix`]). Keeping
    /// one tick count rather than a bool and an angle is what stops the tint and the
    /// topple disagreeing about when death began.
    ///
    /// Not to be confused with `camera_rig`'s own `death_roll_degrees`, which is a
    /// *different* vanilla expression (`GameRenderer.bobHurt`'s
    /// `40 - 8000/(min(deathTime, 20) + 200)`) on the same input: that one rolls the
    /// local player's **camera** when *they* die, this one topples an entity's
    /// **model**. Both are driven by a `deathTime` and they are not interchangeable.
    pub death_time: f32,
    /// This entity's using-item state, when it has ever reported the
    /// `LivingEntity` flags byte — `None` otherwise, like every other component
    /// bridged off the ingest entity.
    ///
    /// # Why the draw needs it and not just [`Self::anim`]
    ///
    /// [`arm_pose_for`] already folds this into `anim.arm_pose`, and that is
    /// enough for the *arms*. It is not enough for the **item**: an item's
    /// definition tree branches on `minecraft:using_item` and dispatches on
    /// `minecraft:use_duration`, so a drawn bow is a different *model*
    /// (`item/bow_pulling_0/1/2`) and not just a different pose.
    /// [`ArmPose::BowAndArrow`] carries no tick count, so it cannot tell
    /// `bow_pulling_0` from `_2` — which is exactly the flattening
    /// [`lodestone_render::ItemVariants`] exists to undo.
    ///
    /// `off_hand` is load-bearing here in a way it is not for the pose: vanilla's
    /// `using_item` property is
    /// `owner.isUsingItem() && owner.getUseItem() == itemStack`, so
    /// `RenderState::merge_held_items` must compare it against the arm it is
    /// drawing or a skeleton drawing a bow would bend its off-hand item too.
    pub item_use: Option<ItemUse>,
    /// `Mob.getMainArm() == HumanoidArm.LEFT`, i.e.
    /// [`lodestone_ecs::entity::MobState::left_handed`]. `false` (right-handed)
    /// for every entity that has never reported the mob-flags byte, same as
    /// [`AnimInput::aggressive`](lodestone_render::entity_anim::AnimInput::aggressive).
    ///
    /// Flips which physical arm both [`arm_pose_for`]'s pose and the held-item
    /// mesh resolve to: a left-handed mob's main-hand item and its ranged pose
    /// both belong on its left arm, not its right. Every equipment-slot → `Arm`
    /// mapping in `gpu/entity_passes.rs`/`gpu/world_items.rs` must XOR against
    /// this rather than assume `Mob.getMainArm()` is always `RIGHT`.
    pub main_arm_left: bool,
    /// A creeper's pre-detonation swell, `0.0..~1.07`, vanilla's
    /// `Creeper.getSwelling(partialTick)` — `0.0` (and hence
    /// [`lodestone_render::entity_anim::Skeleton::pose_swelling`]'s exact
    /// identity case) for every non-creeper, and for a creeper whose fuse is
    /// unlit. Interpolated from [`CreeperFuse::old_swell`]/`swell` by this
    /// frame's partial tick, the same way [`Self::feet`] etc. are.
    ///
    /// This one field feeds **two** consumers downstream (both in `gpu.rs`,
    /// not this module): the whole-model scale
    /// (`Skeleton::pose_swelling`/`creeper_swell_scale`) and, via
    /// [`lodestone_render::entity_anim::creeper_white_overlay_progress`] and
    /// [`lodestone_render::entity_pipeline::creeper_overlay_alpha_from_progress`],
    /// the white-flash overlay — see those two functions' docs for why one
    /// swelling value is enough to drive both.
    pub creeper_swelling: f32,
    /// `LivingEntity.swimAmount`, interpolated for this frame — a `0..1` ramp
    /// toward the swim pose, `0.0` for every entity that has never reported
    /// [`Pose`]`::`[`Swimming`](lodestone_model::EntityPose::Swimming). Mirrors
    /// [`lodestone_physics::player::PlayerState::swim_amount`] for a network
    /// entity we do not run physics for; see [`SwimRamp`]/[`tick_swim_ramp`]
    /// for the client-side integration this interpolates between.
    ///
    /// Its only consumer today is the body-pitch rotation
    /// `gpu/entity_passes.rs` applies to a `"player"` [`Self::type_path`] —
    /// see that module for why only the player is ported (vanilla's own
    /// `LivingEntityRenderer.setupRotations` has no swim branch at all; only
    /// `AvatarRenderer` and `DrownedRenderer` override it, with two different
    /// formulas, and this field only drives the one this build implements).
    pub swim_amount: f32,
    /// Whether this entity's shared-flags byte reports bit `0x01` — vanilla's
    /// `displayFireAnimation()` gate, `Entity.isOnFire() && !isSpectator()`
    /// (`.cache/mc/26.2/client-src/net/minecraft/world/entity/Entity.java:
    /// 2666-2668,3255-3256`). Issue #434, player report: "mobs dont show
    /// flames yet".
    ///
    /// Read off [`lodestone_ecs::entity::EntityFlags`] through [`EntityIndex`]
    /// in [`extract_entity_draws`], bridged the **same way** [`Self::hurt`]
    /// and [`Self::item_use`] already are — `false` for an entity that has
    /// never reported the shared-flags byte at all (`EntityFlags` absent),
    /// which is the correct default: an entity metadata has never described
    /// cannot be known to be on fire.
    ///
    /// This deliberately does **not** re-check vanilla's `!isSpectator()`
    /// half of the gate: a remote entity's game mode is not tracked on this
    /// side of the wire, and the server should never set bit `0x01` on a
    /// spectator's own metadata in the first place (spectators are otherwise
    /// invisible to other clients).
    ///
    /// **Not** the first-person full-screen fire overlay
    /// (`gpu/screen_effects.rs`'s `Vitals::on_fire`, via `ingest.rs`'s
    /// `apply_local_player_on_fire`) — that is a different byte read for a
    /// different, local-player-only purpose, and this field must never feed
    /// it. See `docs/entity-rendering.md`'s "Mob fire" section.
    pub on_fire: bool,
    /// Bit `0x20` of the same shared-flags byte [`Self::on_fire`] reads bit
    /// `0x01` of — vanilla's `Entity.isInvisible()`. Bridged off the ingest
    /// entity's `EntityFlags` through `EntityIndex` in
    /// [`extract_entity_draws`], the same way `on_fire` is; `false` for an
    /// entity that has never reported the byte.
    ///
    /// Gates only the entity's own body/rig — `RenderState::prepare_entities`
    /// (`gpu/entity_passes.rs`) skips the model batch entirely when this is
    /// set, matching `LivingEntityRenderer.submit`'s `isBodyVisible` gate on
    /// its `submitModel` call. Armour and held items are unaffected: they are
    /// drawn by `prepare_armour`/`merge_held_items`/`special_item_instances`,
    /// each of which re-resolves the entity's pose independently rather than
    /// reusing the body pass's instance, matching vanilla's own
    /// `shouldRenderLayers` running unconditionally regardless of body
    /// visibility. The nametag pass reads this same `entities` slice too and
    /// is equally untouched by the body-batch skip — an invisible, named
    /// entity still shows its tag, which is the "server hologram" case issue
    /// #643 reports (an invisible, custom-named armour stand).
    ///
    /// **Not implemented, on purpose, rather than half-built:**
    /// `state.isInvisibleToPlayer` — vanilla still shows an invisible entity,
    /// translucently, to a spectator or a teammate whose team has
    /// `canSeeFriendlyInvisibles`. This draw site has no notion of the
    /// *local* viewer's own game mode, and doing this faithfully needs a
    /// translucent render path this renderer does not have. The glowing
    /// outline (bit `0x40`, `Entity.isCurrentlyGlowing()`) is the same story
    /// — it needs a real outline pass — and is left decoded-and-unread on the
    /// shared-flags byte rather than added here as a field with no consumer,
    /// which is the exact island shape this repo's evidence standards call
    /// out.
    pub invisible: bool,
    /// This entity's own `ArmorStand.DATA_CLIENT_FLAGS` byte, `None` for
    /// every non-`armor_stand` type and for one that has never reported it.
    /// Bridged off the ingest entity's
    /// [`lodestone_ecs::entity::ArmorStandFlags`] through `EntityIndex` in
    /// [`extract_entity_draws`], the same way [`Self::invisible`] is.
    ///
    /// `small` is not read from here a second time — [`resolve_entity_facts`]
    /// already folds it into [`Self::scale`] (a uniform half-scale,
    /// approximating vanilla's separate small-model bake). `show_arms` and
    /// `no_base_plate` are consumed in `gpu/entity_passes.rs`'s
    /// `prepare_entities`, which collapses the named part's own matrix to a
    /// point instead of drawing it — the corpus's `armor_stand` model has
    /// real `left_arm`/`right_arm`/`base_plate` parts to hide, matching
    /// vanilla's `ArmorStandModel.setupAnim` toggling `ModelPart.visible` on
    /// the same three. `marker` has no consumer: vanilla's own use of it is a
    /// render-type switch (`ArmorStandRenderer.getRenderType`, cutout instead
    /// of the default humanoid render type) with no equivalent pipeline state
    /// here, so it stays decoded-and-unread rather than approximated.
    pub armor_stand: Option<lodestone_ecs::entity::ArmorStandFlags>,
    /// This player's declared skin, carried through from
    /// [`EntityFacts::player_skin`] — `None` for every non-player.
    ///
    /// Two consumers, and they must agree: [`Self::model_type_path`] picks the
    /// **rig** from `model`, and `RenderState::prepare_entities` groups by `url`
    /// so the batch carries the **sheet**. A slim-authored sheet on the wide rig
    /// puts the arm UVs a texel out; the wide sheet on the slim rig leaves a gap
    /// at the shoulder. Neither reads as a model bug.
    pub player_skin: Option<crate::remote_skins::RemoteSkin>,
    /// The **variant** texture sheet this entity resolves to — a corpus reference
    /// like `entity/wolf/wolf_ashen` — or `None` for an entity whose model has no
    /// variant axis, or whose reported variant carries nothing that axis can use.
    ///
    /// Resolved once per poll in [`extract_entity_draws`] by
    /// [`lodestone_render::entity_variant_sheet_for`], which is
    /// `EntityTexture::resolve`'s **first production caller**: the corpus has
    /// modelled nine wolf breeds and three climate skins the whole time, and every
    /// consumer asked only for `default_path()`, so every wolf drew pale and every
    /// pig drew temperate. A function with zero production *readers* is the dual of
    /// this repo's usual island, and a connectedness scan cannot see it — the packet
    /// decodes, the fold lands on a component, and nothing downstream asks.
    ///
    /// Consumed exactly like [`Self::player_skin`]: it joins the draw-grouping key
    /// so one batch is one sheet, and a *miss* in the shell's variant texture map
    /// falls back to the model's own sheet rather than failing. Two mobs of the same
    /// species and different breeds are therefore two batches, which is what vanilla
    /// pays too — its `getTextureLocation` is per entity.
    ///
    /// **A wolf's tame state is part of this** (issue #235):
    /// [`extract_entity_draws`] bridges [`lodestone_ecs::entity::Tamed`] off the
    /// ingest entity, the same way it bridges `Variant`, and passes it through to
    /// `entity_variant_sheet_for`'s `tamed` parameter — see that function's own doc
    /// for the wire chain and for why only a wolf's sheet reads the bit.
    pub variant_sheet: Option<&'static str>,
    /// An experience orb's XP value (`ExperienceOrb.DATA_VALUE`), bridged off the
    /// ingest entity's [`ExperienceOrbValue`] component — `None` for every entity
    /// that is not an orb, which is the switch the orb pass keys on.
    ///
    /// Its only consumer is `lodestone_render::experience_orb_icon`, which buckets
    /// it into one of eleven sprite cells. `Some(0)` and `None` therefore draw the
    /// same cell, and that is correct rather than sloppy: vanilla's accessor
    /// default *is* `0`, so an orb whose value never reached us looks exactly like
    /// an orb genuinely worth nothing. What must not happen is the two collapsing
    /// the other way — `None` reading as "not an orb" for a real orb, which draws
    /// nothing at all.
    ///
    /// **Not the orb's `count`.** Vanilla keeps `value` (what one absorption pays,
    /// synced) and `count` (how many absorptions the entity holds after merging,
    /// server-only) as two different numbers, and only the first is on the wire.
    pub experience_orb_value: Option<i32>,
    /// This frame's interpolated `(capeLean, capeLean2, capeFlap)`, all
    /// degrees — see [`cape_sway`] for the derivation and [`CapeLag`] for the
    /// per-tick state it comes from. Computed for every tracked entity (the
    /// state is cheap — see [`CapeLag`]'s doc), consumed only when
    /// [`Self::type_path`] is `"player"` and [`Self::player_skin`] declares a
    /// cape, exactly like [`Self::swim_amount`]'s single player-only reader.
    pub cape_sway: (f32, f32, f32),
}

impl EntityDraw {
    /// The corpus name to resolve this entity's mesh from — [`Self::type_path`]
    /// for everything except a player whose skin declares the **slim** rig.
    ///
    /// A separate accessor rather than rewriting `type_path` at the fold, and
    /// that distinction is load-bearing: `type_path` is also what
    /// `gpu/nametag.rs` feeds to `entity_dimensions::base_dimensions` to place
    /// the tag above the head, and `"player_slim"` is **not** an entity-type
    /// registry path — it would miss, fall back to `FALLBACK_HEIGHT`, and put
    /// every slim player's nametag at the wrong height. `world_items.rs`,
    /// `debug_lines.rs` and the flame pass read it the same way.
    ///
    /// `lodestone_render::entity::player_model_name` is the one place the two
    /// rig names live; both are first-class corpus entries, so
    /// `canonical_model_name` resolves the literal with no extra plumbing.
    #[must_use]
    pub fn model_type_path(&self) -> &str {
        match &self.player_skin {
            Some(skin) if skin.model == lodestone_assets::PlayerModelType::Slim => {
                lodestone_render::entity::player_model_name(true)
            }
            _ => &self.type_path,
        }
    }
}

// ---------------------------------------------------------------------------
// The render-side component set
// ---------------------------------------------------------------------------

/// The entity type's canonical path, as [`resolve_entity_facts`] reported it.
///
/// Distinct from `lodestone_ecs::entity::EntityKind` (a `ResourceKey`) because
/// this is the bare path string `lodestone-render`'s model set is keyed by,
/// while `EntityKind` is the network vocabulary. `EntitySnapshot` (issue #36)
/// is gone, but collapsing these two into one component is a separate,
/// larger change nothing here requires — `RenderKind` still exists
/// specifically so this module needs no `ResourceKey`-shaped lookup on every
/// extract.
///
/// `Arc<str>` rather than `String` (issue #523): `extract_entity_draws` reads
/// this component into a fresh `EntityDraw` every rendered frame for every
/// tracked entity, and a `String` clone there was a per-frame heap allocation
/// plus byte copy for a value that only actually changes on a rare
/// `update_track` fold. `Arc::clone` is a refcount bump; the allocation now
/// happens once, in `spawn_track`/`update_track`, not once per frame per
/// entity.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct RenderKind(pub Arc<str>);

/// Uniform render scale (baby mobs are drawn smaller).
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct RenderScale(pub f32);

/// The pose an ease is coming *from* — re-anchored to whatever was on screen at
/// the moment a fresh target arrived, which is what keeps motion C0-continuous
/// instead of jumping.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct InterpFrom {
    /// Feet position in world space.
    pub feet: Vec3,
    /// Body yaw in degrees.
    pub yaw: f32,
    /// Absolute head yaw in degrees.
    pub head_yaw: f32,
    /// Head pitch in degrees.
    pub pitch: f32,
}

/// The latest reported pose an ease is heading *to*.
///
/// For an entity with [`ItemPhysics`] this is advanced by the local simulation
/// every tick, **not** by the sparse network packet — see the module docs.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct InterpTo {
    /// Feet position in world space.
    pub feet: Vec3,
    /// Body yaw in degrees.
    pub yaw: f32,
    /// Absolute head yaw in degrees.
    pub head_yaw: f32,
    /// Head pitch in degrees.
    pub pitch: f32,
}

/// How far through the current ease we are, and the entity's continuous age.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct InterpClock {
    /// Seconds since the ease was last re-anchored, capped at [`Self::window`].
    pub t: f32,
    /// Continuous age in ticks (`ageInTicks`), driving idle bob.
    pub age: f32,
    /// The real-time length of the current ease. [`INTERP_WINDOW`] for every
    /// network-reported entity — three ticks of slack absorbs the jitter
    /// between one `MOVE_ENTITY` and the next, which really does arrive at
    /// irregular real-time spacing. The vehicle we are currently driving has
    /// no such jitter: [`interp_window_for`] narrows this to one [`TICK`] for
    /// it, because `lodestone_ecs::vehicle::tick_controlled_vehicle` writes
    /// its `Position` locally, exactly once, every single physics tick, with
    /// nothing to smooth. Stacking the network window on top of an
    /// already-tick-quantized source compounds: each tick's re-anchor only
    /// closes `TICK / INTERP_WINDOW` (a third) of the remaining distance
    /// before the *next* tick moves the target again, so under sustained
    /// motion the eased draw position never catches up — see
    /// `riding_render_seat`'s doc for what that looks like on screen (the
    /// seat, which reads this exact eased position, permanently trailing the
    /// vehicle's true tick-boundary motion). At `window == TICK`, `alpha`
    /// reaches `1.0` in exactly the time before the next re-anchor, so the
    /// draw position coincides with the tick-boundary target every tick
    /// rather than only in the limit.
    pub window: f32,
}

impl Default for InterpClock {
    /// `window` defaults to [`INTERP_WINDOW`] — the network-smoothing case,
    /// and the only one a bare `Default::default()` (no
    /// `lodestone_ecs::vehicle::ControlledVehicle` resource in scope to
    /// narrow it) can mean.
    fn default() -> Self {
        Self {
            t: 0.0,
            age: 0.0,
            window: INTERP_WINDOW,
        }
    }
}

/// Vanilla's `WalkAnimationState`, ticked at 20 Hz by [`tick_walk_animation`].
#[derive(Component, Debug, Clone, Copy)]
pub struct WalkAnim {
    /// The animation state itself.
    pub walk: WalkAnimation,
    /// The drawn position at the previous 20 Hz tick. The distance between this
    /// and the current drawn position *is* the per-tick travel
    /// [`walk_target_speed`] wants — see the module note on why the eased gap is
    /// not.
    pub last_feet: Vec3,
}

/// A dropped item's client-run physics: the same gravity/drag [`ItemMotion`] the
/// server itself steps, advanced once per real 20 Hz tick and corrected toward
/// each authoritative server report rather than driven by it.
///
/// **Present only on entities whose [`RenderKind`] is
/// [`ITEM_ENTITY_TYPE_PATH`].** Every other entity type has no such component,
/// which is what keeps it on the original pure position ease — the absence is
/// the switch.
#[derive(Component, Debug, Clone, Copy)]
pub struct ItemPhysics {
    /// The locally-simulated position/velocity.
    pub sim: ItemMotion,
    /// The most recently reported *authoritative* feet position — kept separate
    /// from [`InterpTo::feet`], which the simulation itself advances every tick,
    /// so re-polling the same still-current server value doesn't look like a
    /// fresh "moved" event every frame.
    pub last_reported: Vec3,
    /// Whether the last-reported snapshot said the item is resting. The
    /// simulation is paused while `true` — a resting item does not need
    /// resimulating every tick, and this avoids any drift between the local
    /// collision result and the server's own resting position. See
    /// [`step_item_physics`] for the (collision-aware) airborne case.
    pub grounded: bool,
}

/// Vanilla's `Creeper.DEFAULT_MAX_SWELL` (`Creeper.java`): the tick count a
/// fuse counts up to before [`tick_creeper_fuse`] treats it as fully swollen.
/// A creeper's real `maxSwell` can differ (the `Fuse` NBT tag), but that value
/// is never synchronised to the client — see
/// [`lodestone_render::entity_anim::MAX_SWELL`]'s doc for the same constant on
/// the render side (`/ 28`, not `/ 30`, is `maxSwell - 2`).
const CREEPER_MAX_SWELL_TICKS: i32 = 30;

/// A creeper's fuse, integrated **client-side** one tick at a time from the
/// synced [`EntityFacts::creeper_swell_dir`] — exactly what vanilla's own
/// client does (`Creeper.java`, `this.swell += swellDir`), because only
/// the *direction* is ever on the wire, never the counter itself. See
/// [`lodestone_render::entity_anim::pose_swelling`]'s doc for the full
/// derivation of why this split exists.
///
/// **Present only on entities whose [`RenderKind`] is `"creeper"`** — the
/// same "absence is the switch" pattern [`ItemPhysics`] uses, so every other
/// entity type carries no cost from this at all.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreeperFuse {
    /// The last-synced fuse direction: `-1` idle/backing off, `1` counting up
    /// to detonation. [`tick_creeper_fuse`] only ever reads this;
    /// [`update_track`] is what writes it, from a snapshot that reported one.
    pub swell_dir: i32,
    /// The integrated counter one tick ago — what [`extract_entity_draws`]
    /// interpolates *from*, exactly as `Creeper.oldSwell` does.
    pub old_swell: i32,
    /// The integrated counter as of the most recent tick — what
    /// [`extract_entity_draws`] interpolates *to*.
    pub swell: i32,
}

impl CreeperFuse {
    /// Vanilla's own accessor default (`Creeper.java`,
    /// `entityData.define(DATA_SWELL_DIR, -1)`): idle, nothing swollen. Used
    /// to seed a freshly spawned creeper before its first metadata report —
    /// see [`spawn_track`].
    pub const IDLE: Self = Self {
        swell_dir: -1,
        old_swell: 0,
        swell: 0,
    };
}

/// `GameTick` / `TickSet::Animate`: integrates every tracked creeper's fuse by
/// exactly one tick, byte-for-byte `Creeper.java`'s
/// `this.swell += swellDir` (clamped `0..=maxSwell` the same way `tick()`
/// clamps it — `Creeper.java`). Run client-side because only
/// [`CreeperFuse::swell_dir`] is ever synced; the counter itself is not.
pub fn tick_creeper_fuse(mut fuses: Query<&mut CreeperFuse>) {
    for mut fuse in &mut fuses {
        fuse.old_swell = fuse.swell;
        fuse.swell = (fuse.swell + fuse.swell_dir).clamp(0, CREEPER_MAX_SWELL_TICKS);
    }
}

/// `LivingEntity.swimAmount`, integrated **client-side** one tick at a time —
/// the render-track counterpart of [`CreeperFuse`], and the same reason it
/// exists: only the *pose* (`Pose.SWIMMING`, at metadata index 6) is ever on
/// the wire, never the ramp itself, so [`tick_swim_ramp`] has to advance it
/// here exactly as `LivingEntity.updateSwimAmount()` does
/// (`LivingEntity.java:3478-3483`) rather than reading a synced value.
///
/// **Present on every track entity**, not gated by [`RenderKind`] the way
/// [`CreeperFuse`] is gated to `"creeper"` — vanilla's swim rotation is not
/// species-specific machinery the way a creeper's swell is (see
/// [`crate::gpu::entity_passes`]'s swim rotation for the one species this
/// build actually ports it for), and an entity that never reports
/// `Pose.SWIMMING` just sits at `0.0` forever, which costs nothing to carry.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct SwimRamp {
    /// The last-synced pose, read from [`Pose`] at metadata index 6 every
    /// tick. [`tick_swim_ramp`] both reads and writes this component, unlike
    /// [`CreeperFuse::swell_dir`] which [`update_track`] writes from a
    /// snapshot — the ingest [`Pose`] component is already the up-to-date
    /// synced value, so there is no separate fold step to bridge it through.
    pub swimming: bool,
    /// The integrated ramp one tick ago — what [`extract_entity_draws`]
    /// interpolates *from*, exactly as `LivingEntity.swimAmountO` does.
    pub old: f32,
    /// The integrated ramp as of the most recent tick — what
    /// [`extract_entity_draws`] interpolates *to*.
    pub current: f32,
}

impl SwimRamp {
    /// A freshly tracked entity starts unswum, exactly as [`spawn_track`]
    /// starts every other ease at rest: both ends of the ramp are `0.0`, so a
    /// newly seen entity that happens to already be mid-swim ramps up over
    /// the next ~11 ticks rather than snapping to a bent pose on its first
    /// frame.
    pub const IDLE: Self = Self {
        swimming: false,
        old: 0.0,
        current: 0.0,
    };
}

/// `LivingEntity.updateSwimAmount()`, run against the last pose
/// [`tick_swim_ramp`] itself read off the ingest [`Pose`] component — advances
/// by `SWIM_AMOUNT_PER_TICK` (`0.09F`, the same constant
/// `lodestone_physics::player::update_swim_amount` uses for the local player)
/// toward `1.0` while swimming, back toward `0.0` otherwise, clamped both
/// ends.
///
/// `GameTick` / `TickSet::Animate`, beside [`tick_creeper_fuse`]. Bridges
/// [`Pose`] off the *ingest* entity through [`EntityIndex`] itself, the same
/// way [`extract_entity_draws`] bridges [`AttackSwing`]/[`HurtTime`]/etc. —
/// this system, not a fold through [`EntityFacts`], is the source of truth
/// for "is this entity swimming right now".
pub fn tick_swim_ramp(
    index: Res<EntityIndex>,
    poses: Query<&Pose>,
    mut ramps: Query<(&MinecraftEntityId, &mut SwimRamp)>,
) {
    const SWIM_AMOUNT_PER_TICK: f32 = 0.09;
    for (id, mut ramp) in &mut ramps {
        ramp.swimming = index
            .get(id.0)
            .and_then(|entity| poses.get(entity).ok())
            .is_some_and(|pose| pose.0 == lodestone_model::EntityPose::Swimming);
        ramp.old = ramp.current;
        ramp.current = if ramp.swimming {
            (ramp.current + SWIM_AMOUNT_PER_TICK).min(1.0)
        } else {
            (ramp.current - SWIM_AMOUNT_PER_TICK).max(0.0)
        };
    }
}

/// The lagged "cloak" position vanilla's `ClientAvatarState` tracks per
/// avatar (`xCloak`/`yCloak`/`zCloak`, `26.2`) — the position the cape's
/// pivot chases, easing 25% of the remaining gap toward the entity's real
/// per-tick position every tick, with a 10-block teleport snap. The gap
/// between this lagged point and the entity's real position, resolved
/// against body yaw, is what makes a cape swing wide on a turn and trail
/// behind on a sprint — see [`tick_cape_lag`] and [`cape_sway`].
///
/// **Present on every track entity**, the same "costs nothing to carry"
/// choice [`SwimRamp`] makes: the extra state is three `f64` pairs and one
/// `f32` pair, and gating it by [`RenderKind`] would need the same
/// "well-known but changeable skin url" plumbing [`RenderPlayerSkin`] already
/// carries, for no measurable win — only a `"player"` [`EntityDraw::type_path`]
/// ever reads the derived sway, exactly like [`EntityDraw::swim_amount`]'s
/// player-only consumer.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct CapeLag {
    /// The lagged position, current tick.
    pub cloak: Vec3,
    /// The lagged position, one tick ago — what [`extract_entity_draws`]
    /// interpolates *from*.
    pub cloak_o: Vec3,
    /// The real per-tick position [`tick_cape_lag`] last eased toward
    /// (`InterpTo::feet`), kept so the *next* tick can compute a fresh delta
    /// without re-reading the query tuple's `InterpTo` a second time.
    pub last_feet: Vec3,
    /// `ClientAvatarState.bob`: an eased 0..0.1 walk-bob amplitude,
    /// `AbstractClientPlayer.updateBob`'s `tBob` (this tick's horizontal
    /// travel, clamped to `0.1`, zeroed while swimming) eased by `0.4` per
    /// tick toward the target.
    pub bob: f32,
    /// `bob`, one tick ago — what [`extract_entity_draws`] interpolates
    /// *from*.
    pub bob_o: f32,
}

impl CapeLag {
    /// A freshly tracked entity starts with its cloak pinned to wherever it
    /// first appears — `ClientAvatarState`'s fields all default to `0.0`, but
    /// unlike vanilla (which only ever constructs one per real player, at a
    /// real position) a spawned track can appear anywhere, so pinning here
    /// avoids one tick of the cape lunging in from the world origin. `bob`
    /// starts at rest, same as [`SwimRamp::IDLE`].
    pub fn at(feet: Vec3) -> Self {
        Self {
            cloak: feet,
            cloak_o: feet,
            last_feet: feet,
            bob: 0.0,
            bob_o: 0.0,
        }
    }
}

/// `ClientAvatarState.moveCloak` + `AbstractClientPlayer.updateBob`, one 20 Hz
/// step, for every tracked entity — cheap enough (see [`CapeLag`]'s doc) not
/// to gate by type, and it must run at tick rate: both are per-tick eases in
/// vanilla, not per-frame ones, exactly like [`tick_swim_ramp`].
///
/// **Approximation, stated rather than hidden:** vanilla's `tBob` also gates
/// on `!isDeadOrDying()`, which would need [`DeathTime`] bridged through
/// [`EntityIndex`] the way [`OnGround`]/[`Pose`] are below; a dying entity's
/// cape bob not freezing on the killing blow is the one behaviour this port
/// does not chase, in exchange for not widening this query further.
pub fn tick_cape_lag(
    index: Res<EntityIndex>,
    grounded: Query<&OnGround>,
    poses: Query<&Pose>,
    mut tracks: Query<(&MinecraftEntityId, &InterpTo, &mut CapeLag)>,
) {
    const EASE: f32 = 0.25;
    const TELEPORT_THRESHOLD: f32 = 10.0;
    const BOB_EASE: f32 = 0.4;
    const MAX_BOB_TARGET: f32 = 0.1;

    for (id, to, mut lag) in &mut tracks {
        lag.cloak_o = lag.cloak;
        let delta = to.feet - lag.cloak;
        // Vanilla checks each axis independently, so a huge single-axis
        // teleport (through a portal, say) snaps only what actually jumped —
        // reproduced faithfully rather than snapping all three together.
        let ease_axis = |gap: f32, cur: f32, target: f32| {
            if gap.abs() > TELEPORT_THRESHOLD {
                target
            } else {
                cur + gap * EASE
            }
        };
        lag.cloak = Vec3::new(
            ease_axis(delta.x, lag.cloak.x, to.feet.x),
            ease_axis(delta.y, lag.cloak.y, to.feet.y),
            ease_axis(delta.z, lag.cloak.z, to.feet.z),
        );

        let horizontal = (to.feet - lag.last_feet).with_y(0.0).length();
        lag.last_feet = to.feet;
        let entity = index.get(id.0);
        let swimming = entity
            .and_then(|e| poses.get(e).ok())
            .is_some_and(|pose| pose.0 == lodestone_model::EntityPose::Swimming);
        let on_ground = entity.and_then(|e| grounded.get(e).ok()).is_some_and(|g| g.0);
        let bob_target = if on_ground && !swimming {
            horizontal.min(MAX_BOB_TARGET)
        } else {
            0.0
        };
        lag.bob_o = lag.bob;
        lag.bob += (bob_target - lag.bob) * BOB_EASE;
    }
}

/// Vanilla's `AvatarRenderer.extractCapeState` (`26.2`), given this frame's
/// interpolated cloak lag and body yaw: the `(capeLean, capeLean2, capeFlap)`
/// triple [`lodestone_render::entity::cape_local_rotation`] turns into a
/// rotation.
///
/// `body_yaw_deg` must be the **body** yaw (not head yaw) — vanilla derives
/// `forwardX`/`forwardZ` from `yBodyRot`, using [`lodestone_physics::mth`]'s
/// quantised sin/cos rather than `f32::sin`/`cos` (this repo's own rule: the
/// two diverge at cardinal angles, and a body yaw of exactly `0`/`90`/`180`/
/// `270` is not a rare fixture here — it is spawn-facing).
///
/// `fall_flying_scale` (vanilla multiplies `capeLean` by
/// `1.0 - state.fallFlyingScale()`) is not threaded through: no draw in this
/// codebase currently resolves elytra-flight scale for a remote entity, so
/// this is the identity case (`fall_flying_scale == 0.0`) unconditionally —
/// correct for every grounded/walking/swimming player, and a slightly wider
/// lean than vanilla for one actively gliding.
#[must_use]
pub fn cape_sway(delta: Vec3, body_yaw_deg: f32, bob: f32, walk_distance: f32) -> (f32, f32, f32) {
    let yaw = f64::from(body_yaw_deg.to_radians());
    let forward_x = mth::sin(yaw);
    let forward_z = -mth::cos(yaw);
    let flap_lag = (delta.y * 10.0).clamp(-6.0, 32.0);
    let lean = ((delta.x * forward_x + delta.z * forward_z) * 100.0).clamp(0.0, 150.0);
    let lean2 = ((delta.x * forward_z - delta.z * forward_x) * 100.0).clamp(-20.0, 20.0);
    let flap = flap_lag + mth::sin(f64::from(walk_distance * 6.0)) * 32.0 * bob;
    (lean, lean2, flap)
}

/// The occupied equipment slots, narrowed from [`EntityFacts::equipment`].
///
/// A component rather than a side table (as [`ItemStacks`] is) precisely
/// *because* it is replaced wholesale every poll: there is no "reported once,
/// then silence" hazard to guard against, so there is nothing for a separate
/// table's prune to protect, and hanging it on the entity means a despawn prunes
/// it for free.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct RenderEquipment(pub Vec<(EquipmentSlot, ResourceLocation)>);

/// Per-slot `minecraft:dyed_color`, narrowed from
/// [`EntityFacts::equipment_dye`] — a separate component from
/// [`RenderEquipment`] rather than a wider tuple inside it, for the same
/// reason the snapshot field is additive; see that field's doc.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderEquipmentDye(pub Vec<(EquipmentSlot, u32)>);

/// Per-slot `minecraft:trim`, narrowed from [`EntityFacts::equipment_trim`] — a
/// third component beside [`RenderEquipment`] and [`RenderEquipmentDye`] for
/// their reason, and because a piece can be dyed and trimmed at once (issue #17).
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderEquipmentTrim(pub Vec<(EquipmentSlot, lodestone_model::item::ArmorTrim)>);

/// This entity's sheep-wool state, narrowed from [`EntityFacts::variant`] by
/// [`sheep_wool`].
///
/// A component for the same reason [`RenderEquipment`] is one rather than a
/// side table: it is replaced wholesale every poll (shearing is a metadata
/// update, not a movement), so there is no "reported once, then silence"
/// hazard and a despawn prunes it for free.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderWool(pub Option<SheepWool>);

/// This entity's resolved nametag (issue #100), narrowed from
/// [`EntityFacts::name_tag`].
///
/// A component for the same reason [`RenderEquipment`]/[`RenderWool`] are:
/// replaced wholesale every poll (a player's tab-list name can change, a
/// mob's `CUSTOM_NAME_VISIBLE` can toggle, both with no movement at all), so
/// there is no "reported once, then silence" hazard and a despawn prunes it
/// for free.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderNameTag(pub Option<NameTag>);

/// This player's declared skin, narrowed from [`EntityFacts::player_skin`].
///
/// A component for [`RenderNameTag`]'s reason, and the "replaced wholesale every
/// poll" part matters more here than anywhere else in this list: **the profile
/// routinely arrives after the entity does.** A player's `ADD_PLAYER` and their
/// `ADD_ENTITY` are separate packets, so the first few folds of a remote player
/// legitimately see no tab-list entry at all and this is `None` — it has to be
/// allowed to become `Some` later, which a spawn-time-only insert would forbid.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderPlayerSkin(pub Option<crate::remote_skins::RemoteSkin>);

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// This frame's elapsed seconds, read by [`advance_interp_clocks`].
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct FrameDelta(pub f32);

/// The collision geometry **dropped items** are simulated against this tick.
///
/// # Why this is not the player's `PlayerCollision`
///
/// It was the same type in a *different* `World` before §4.1(c), and unifying the
/// `World`s would have silently merged two genuinely different decisions:
///
/// | case | the player's `PlayerCollision` | this |
/// |---|---|---|
/// | live, the player's column not streamed yet | `Pending` — hold the player still rather than drop them | fall back to the chunk store; an item elsewhere still has a floor |
/// | `collide_against_live_world = false` (the live gate's negative control) | an explicitly **empty** store, so the player falls through | the real chunk store, so the control does not accidentally also disable item physics |
///
/// `docs/sim-dissolution.md` recorded this as the reason `tick_particles` stayed a
/// method; the same reasoning applies here, and the answer is a second resource
/// with its own documented decision rather than one resource with two meanings.
#[derive(Resource, Debug, Default)]
pub struct ItemCollision(pub PlayerCollision);

/// One dropped-item entity's carried stack: the item's identity plus how many
/// are in it. Kept together so [`ItemStacks`] cannot record a count with no
/// matching identity or vice versa.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedStack {
    id: ResourceLocation,
    count: u32,
    /// Whether the stack is enchanted, so the drop draws the glint second pass.
    foil: bool,
    /// Mirrors [`EntityFacts::item_dyed_color`] — carried through so a pickup
    /// flight ([`PickupAnimation`]) keeps the real tint of the stack it froze,
    /// instead of the item definition's plain default.
    dyed_color: Option<u32>,
    /// Mirrors [`EntityFacts::item_potion_color`].
    potion_color: Option<u32>,
}

/// Which item (and how many) each dropped-item entity is carrying, keyed by
/// **server** entity id.
///
/// A resource keyed by server id rather than a component, because a caller may
/// learn an item's identity *before* the entity is tracked at all
/// ([`EntityInterpolator::set_item_stack`] is a public seam and the live path's
/// metadata can precede the snapshot poll). Pruned alongside the tracks, so a
/// despawned drop leaves nothing behind.
#[derive(Resource, Debug, Default)]
pub struct ItemStacks(HashMap<i32, TrackedStack>);

/// Server entity id → the ECS entity holding its render components.
#[derive(Resource, Debug, Default)]
pub struct TrackIndex(HashMap<i32, Entity>);

/// This frame's extracted draw list, written by [`extract_entity_draws`] and
/// appended to by [`extract_pickup_draws`].
#[derive(Resource, Debug, Default)]
pub struct ExtractedDraws(Vec<EntityDraw>);

// ---------------------------------------------------------------------------
// The item-pickup fly-to-collector animation (issue #365)
// ---------------------------------------------------------------------------

/// `ItemPickupParticle.LIFE_TIME` — the pickup flight lasts **3 ticks** (150 ms).
///
/// Read from `net/minecraft/client/particle/ItemPickupParticle.java`:
/// `protected static final int LIFE_TIME = 3;`, and `tick()` removes the particle
/// on the tick `life` reaches it.
const PICKUP_LIFE_TICKS: f32 = 3.0;

/// The height above the collector's feet the item flies *to*, as a fraction of
/// the collector's eye height.
///
/// `ItemPickupParticle.updatePosition()` targets
/// `(target.getY() + target.getEyeY()) / 2.0`, and `Entity.getEyeY()` is
/// `position.y + eyeHeight` (`Entity.java`) — an **absolute** Y, not an
/// offset. So the midpoint is `y + eyeHeight / 2`, i.e. this constant times the
/// eye height above the feet. Reading `getEyeY()` as a relative offset instead
/// would target `y + (y + 1.62)/2`, which for a player at y = 64 is 32 blocks
/// below the floor.
const PICKUP_TARGET_EYE_FRACTION: f32 = 0.5;

/// A **remote** collector's assumed eye height, for the
/// [`PICKUP_TARGET_EYE_FRACTION`] midpoint.
///
/// `lodestone_physics::player::DEFAULT_EYE_HEIGHT` is `Avatar.DEFAULT_EYE_HEIGHT`
/// (`1.62`). The local player's own live `PhysicsState` eye height is used instead
/// when the collector *is* us (it tracks the swimming/crawling pose), so this
/// constant only covers other players and mobs — a fox or an allay picking
/// something up aims 0.81 blocks up rather than at its own smaller midpoint. An
/// approximation, and the only one in this animation: the render-side track set
/// carries no per-entity eye height, and inventing one from [`RenderScale`] would
/// be a guess dressed as a measurement.
const REMOTE_COLLECTOR_EYE_HEIGHT: f32 = lodestone_physics::player::DEFAULT_EYE_HEIGHT;

/// One in-flight item-pickup animation: a **frozen copy** of a collected item,
/// travelling from where the item was drawn to the entity that collected it.
///
/// # Why a copy, and not the item entity retargeted
///
/// This is the part that is easy to get backwards. Vanilla does **not** keep the
/// item entity alive and lerp it: `ClientPacketListener.handleTakeItemEntity`
/// extracts the item's render state (`extractEntity(from, 1.0F)`), hands that
/// *copy* to a new `ItemPickupParticle`, and then calls
/// `this.level.removeEntity(packet.getItemId(), RemovalReason.DISCARDED)` in the
/// same breath. The entity is gone before the animation starts; what flies is a
/// snapshot.
///
/// That is also why this is a resource rather than a component: by the time
/// `fold_entities` next runs, the server has stopped reporting the item and the
/// render track is pruned. An animation hung off the track would be despawned
/// with it, one frame in.
#[derive(Debug, Clone, PartialEq)]
pub struct PickupAnimation {
    /// The collected item entity's id, kept only as the bob-phase key
    /// [`lodestone_render::entity::item_bob_offset`] hashes — the same phase the
    /// item had before it was picked up, so the copy does not visibly re-roll.
    pub item_entity_id: i32,
    /// Which item model to draw.
    pub item: ResourceLocation,
    /// The collected stack size, carried for parity with [`EntityDraw::count`].
    pub count: u32,
    /// Whether the collected stack was enchanted, so the flying copy glints too.
    pub foil: bool,
    /// Mirrors [`TrackedStack::dyed_color`], captured at the same instant as
    /// [`Self::foil`] so the flying copy's tint cannot disagree with its glint.
    pub dyed_color: Option<u32>,
    /// Mirrors [`TrackedStack::potion_color`].
    pub potion_color: Option<u32>,
    /// The item's render scale at capture.
    pub scale: f32,
    /// Where the item was **drawn** when the pickup arrived — not its last
    /// reported position. `ItemPickupParticle` is constructed from the extracted
    /// render state, which is the interpolated pose, so this is the same quantity.
    pub start: Vec3,
    /// Frozen `ageInTicks`, so the bob/spin stop the instant the copy is taken —
    /// vanilla's render state is extracted once and never re-extracted.
    pub age_ticks: f32,
    /// The collecting entity's id. **Any** entity, not just the local player:
    /// vanilla animates a mob's pickup too.
    pub collector_id: i32,
    /// Whole ticks elapsed, `0..PICKUP_LIFE_TICKS`.
    pub life: f32,
}

/// Every item-pickup animation currently in flight.
///
/// Started by [`begin_item_pickup`] (from `Sim::poll_net`, off
/// `ClientEvent::ItemPickup`), advanced by [`tick_pickup_animations`] at 20 Hz,
/// and drawn by [`extract_pickup_draws`] — which is the answer to "what consumes
/// this": it appends an ordinary [`EntityDraw`] per animation, so the flight goes
/// through the *existing* dropped-item geometry path
/// (`RenderState::prepare_item_geometry`) with no new pipeline.
#[derive(Resource, Debug, Default)]
pub struct PickupAnimations(Vec<PickupAnimation>);

impl PickupAnimations {
    /// How many animations are in flight.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether nothing is in flight.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The in-flight animations, for a test or a debug overlay.
    #[must_use]
    pub fn animations(&self) -> &[PickupAnimation] {
        &self.0
    }
}

/// Vanilla's `ItemPickupParticleGroup.ParticleInstance.fromParticle` easing:
///
/// ```text
/// time = (life + partialTick) / 3.0;  time *= time;
/// pos  = lerp(time, itemRenderState.pos, targetPos)
/// ```
///
/// So the interpolant is **quadratic in the age fraction** — an ease-*in*: the
/// item leaves slowly and arrives fast. A linear lerp is the obvious wrong
/// reading and is visibly different at the midpoint: at `life + partial = 1.5`
/// the correct fraction is `0.25`, a linear one gives `0.5`.
#[must_use]
fn pickup_progress(life: f32, partial_tick: f32) -> f32 {
    let t = ((life + partial_tick) / PICKUP_LIFE_TICKS).clamp(0.0, 1.0);
    t * t
}

/// Start a pickup animation for `item_entity_id` flying to `collector_id`.
///
/// Returns `false` — and starts nothing — when the item was not tracked on the
/// render side, either because its stack was never reported or because the track
/// has already been pruned. Drawing a flight from a made-up start point would be
/// worse than drawing none, and "no animation" is exactly the pre-#365
/// behaviour rather than a new failure.
///
/// **Must be called before the frame's `fold_entities`.** `Sim::poll_net` runs
/// ahead of `Sim::fold_entities`, so the track the server has stopped reporting
/// is still present here and gone one call later — that ordering is the whole
/// reason this is a function called from `poll_net` rather than a system.
pub fn begin_item_pickup(world: &mut World, item_entity_id: i32, collector_id: i32) -> bool {
    let Some(stack) = world
        .resource::<ItemStacks>()
        .0
        .get(&item_entity_id)
        .cloned()
    else {
        return false;
    };
    let Some(entity) = world
        .resource::<TrackIndex>()
        .0
        .get(&item_entity_id)
        .copied()
    else {
        return false;
    };
    let Some((start, age_ticks, scale)) = world.get_entity(entity).ok().and_then(|entity| {
        let from = entity.get::<InterpFrom>()?;
        let to = entity.get::<InterpTo>()?;
        let clock = entity.get::<InterpClock>()?;
        let scale = entity.get::<RenderScale>()?;
        Some((render_feet(from, to, clock), clock.age, scale.0))
    }) else {
        return false;
    };
    world
        .resource_mut::<PickupAnimations>()
        .0
        .push(PickupAnimation {
            item_entity_id,
            item: stack.id,
            count: stack.count,
            foil: stack.foil,
            dyed_color: stack.dyed_color,
            potion_color: stack.potion_color,
            scale,
            start,
            age_ticks,
            collector_id,
            life: 0.0,
        });
    true
}

/// `GameTick` / `TickSet::Animate`: one 20 Hz step of every pickup flight.
///
/// `ItemPickupParticle.tick()` is `life++; if (life == 3) remove();` — so an
/// animation is drawn on ticks 0, 1 and 2 and gone on 3. Advancing this per
/// *frame* would make the flight last 3 frames (50 ms at 60 fps), the same
/// frame-rate-dependence `Sim::step`'s note on `chest_lids.tick()` records.
pub fn tick_pickup_animations(mut pickups: ResMut<PickupAnimations>) {
    for pickup in &mut pickups.0 {
        pickup.life += 1.0;
    }
    pickups.0.retain(|pickup| pickup.life < PICKUP_LIFE_TICKS);
}

/// `Extract` / `ExtractSet::Entities`, **after** [`extract_entity_draws`]:
/// append one [`EntityDraw`] per in-flight pickup at its interpolated position.
///
/// This is the consumer that makes the animation reach pixels. It emits a draw
/// whose `type_path` is [`ITEM_ENTITY_TYPE_PATH`], the same one a live dropped item
/// emits, so
/// `RenderState::prepare_item_geometry` picks it up with no change at all on the
/// GPU side — the flight is an existing draw at a new position, not a new pass.
///
/// Ordering matters twice over: [`extract_entity_draws`] **clears**
/// [`ExtractedDraws`], so running before it would have every pickup wiped in the
/// same frame it was written — a green-unit-test island of exactly the shape
/// `CLAUDE.md` §1 describes.
pub fn extract_pickup_draws(
    clock: Res<lodestone_ecs::FrameClock>,
    pickups: Res<PickupAnimations>,
    index: Res<TrackIndex>,
    poses: Query<(&InterpFrom, &InterpTo, &InterpClock)>,
    locals: Query<(&MinecraftEntityId, &PhysicsState), With<LocalPlayer>>,
    mut out: ResMut<ExtractedDraws>,
) {
    if pickups.0.is_empty() {
        return;
    }
    let partial_tick = clock.interp_alpha.clamp(0.0, 1.0);
    for pickup in &pickups.0 {
        let Some(target) = collector_target(pickup.collector_id, &index, &poses, &locals) else {
            continue;
        };
        let feet = pickup.start.lerp(target, pickup_progress(pickup.life, partial_tick));
        out.0.push(EntityDraw {
            id: pickup.item_entity_id,
            type_path: Arc::from(ITEM_ENTITY_TYPE_PATH),
            item: Some(pickup.item.clone()),
            count: pickup.count,
            foil: pickup.foil,
            item_dyed_color: pickup.dyed_color,
            item_potion_color: pickup.potion_color,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_trim: Vec::new(),
            wool: None,
            // A pickup animation is always a dropped item, never a falling block
            // and never an experience orb — the XP an orb pays goes straight to
            // the bar, so there is no flight animation for one to reuse.
            block_state: None,
            item_frame_rotation: 0,
            experience_orb_value: None,
            cape_sway: (0.0, 0.0, 0.0),
            feet,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: pickup.scale,
            anim: AnimInput {
                // Frozen at capture, like the extracted render state it stands
                // in for. Everything else in `AnimInput` is meaningless for an
                // item model, which has no skeleton.
                age_ticks: pickup.age_ticks,
                ..AnimInput::default()
            },
            name_tag: None,
            hurt: false,
            // An item entity is not a `LivingEntity`, so it has no `deathTime` to
            // topple over — vanilla's fall-over lives on `LivingEntityRenderer`, and
            // an item's renderer never calls `setupRotations` at all.
            death_time: 0.0,
            // A flying pickup is an item entity, not a living one: nothing can be
            // using it, so its variant resolves in `DisplaySlot::Ground` alone.
            item_use: None,
            // An item entity is not a `Mob` and has no main arm at all.
            main_arm_left: false,
            // An item entity is never a creeper.
            creeper_swelling: 0.0,
            // An item entity never swims — `Item.updateSwimAmount` is a
            // `LivingEntity` behaviour and `ItemEntity` is not one.
            swim_amount: 0.0,
            // A pickup-flight animation is synthetic (this pass's own item, not
            // a tracked entity with `EntityFlags`) and vanishes in 3 ticks — not
            // worth threading a real flag lookup through for.
            on_fire: false,
            // Same reasoning as `on_fire`: a synthetic pickup-flight item has
            // no `EntityFlags` to read invisible off, and vanishes too fast
            // to matter if it did.
            invisible: false,
            // An item entity is never an armour stand.
            armor_stand: None,
            // An item entity is never a player either.
            player_skin: None,
            variant_sheet: None,
        });
    }
}

/// Where a pickup flies *to*: the collector's `(x, y + eyeHeight/2, z)`.
///
/// Resolved fresh every frame rather than captured at pickup time, because
/// `ItemPickupParticle.updatePosition()` re-reads `target.getX()/getY()` on every
/// tick — a pickup while walking must chase the collector, not aim at where they
/// used to be.
///
/// Two sources, in order, and the local player needs the second one: it has no
/// [`RenderKind`]/[`InterpTo`] render track at all (that absence is deliberate —
/// see `lodestone_ecs::ingest::apply_local_player_login` on why a self-model
/// stays off the render path), so resolving only through [`TrackIndex`] would
/// silently animate nothing for **every pickup the player makes**, which is all
/// of them that matter.
fn collector_target(
    collector_id: i32,
    index: &TrackIndex,
    poses: &Query<(&InterpFrom, &InterpTo, &InterpClock)>,
    locals: &Query<(&MinecraftEntityId, &PhysicsState), With<LocalPlayer>>,
) -> Option<Vec3> {
    for (id, state) in locals {
        if id.0 == collector_id {
            let p = state.0.position;
            return Some(Vec3::new(
                p.x as f32,
                p.y as f32 + state.0.eye_height * PICKUP_TARGET_EYE_FRACTION,
                p.z as f32,
            ));
        }
    }
    let entity = index.0.get(&collector_id).copied()?;
    let (from, to, clock) = poses.get(entity).ok()?;
    let feet = render_feet(from, to, clock);
    Some(feet + Vec3::Y * (REMOTE_COLLECTOR_EYE_HEIGHT * PICKUP_TARGET_EYE_FRACTION))
}

// ---------------------------------------------------------------------------
// Pose readers
// ---------------------------------------------------------------------------

/// The fraction `[0, 1]` through the current interpolation window.
fn alpha(clock: &InterpClock) -> f32 {
    (clock.t / clock.window).clamp(0.0, 1.0)
}

/// The currently-drawn position: [`InterpFrom`] eased toward [`InterpTo`].
fn render_feet(from: &InterpFrom, to: &InterpTo, clock: &InterpClock) -> Vec3 {
    from.feet.lerp(to.feet, alpha(clock))
}

/// The local player's seat position **this frame**, derived from the
/// vehicle's own per-frame interpolated draw pose — [`render_feet`]/
/// [`render_yaw`], the exact functions the vehicle is drawn from — rather
/// than its raw tick-boundary [`lodestone_ecs::entity::Position`]. See
/// [`crate::sim::camera::Sim::interpolated_player`]'s doc for why the two
/// disagree and what that disagreement looks like on screen: the vehicle's
/// on-screen mesh eases toward a target over a real-time window and, under
/// sustained movement, never fully catches up to it; the tick-boundary target
/// itself has no such lag.
///
/// `vehicle_network_id` is [`lodestone_ecs::session::Riding`]'s payload;
/// `own_network_id` is [`lodestone_ecs::session::ServerEntityId`]'s, used
/// only to resolve which of the vehicle's [`lodestone_ecs::entity::Passengers`]
/// seats is ours — the same lookup
/// `lodestone_ecs::player::pin_passenger_to_vehicle` does, repeated here
/// because that function computes a *tick-boundary* seat and this one needs a
/// *per-frame* one, off a different position input.
///
/// # Declines rather than guesses, mirroring `pin_passenger_to_vehicle`
///
/// `None` when any link is missing: the vehicle has no [`EntityIndex`] entry
/// yet (not spawned client-side), no interpolation track
/// ([`InterpFrom`]/[`InterpTo`]/[`InterpClock`]) yet, no
/// [`lodestone_ecs::entity::EntityKind`], or [`lodestone_ecs::VersionData`]
/// holds no adapter or no facts for its type. The caller falls back to the
/// tick-boundary seat in every such case, so declining here never strands the
/// player somewhere invented.
pub(crate) fn riding_render_seat(
    world: &World,
    vehicle_network_id: i32,
    own_network_id: Option<i32>,
) -> Option<Vec3> {
    let index = world.get_resource::<EntityIndex>()?;
    let vehicle = index.get(vehicle_network_id)?;
    let from = world.get::<InterpFrom>(vehicle)?;
    let to = world.get::<InterpTo>(vehicle)?;
    let clock = world.get::<InterpClock>(vehicle)?;
    let kind = world.get::<lodestone_ecs::entity::EntityKind>(vehicle)?;
    let version = world.get_resource::<lodestone_ecs::VersionData>()?;
    let facts = version.entity_facts(&kind.0)?;
    let passengers = world.get::<lodestone_ecs::entity::Passengers>(vehicle);
    // `Entity.getDefaultPassengerAttachmentPoint`: `vehicle.getPassengers().indexOf(passenger)`,
    // the same degenerate-case-agrees reasoning `pin_passenger_to_vehicle`'s
    // own doc gives for defaulting to seat 0.
    let seat_index = own_network_id
        .and_then(|own| passengers.and_then(|list| list.0.iter().position(|id| *id == own)))
        .unwrap_or(0);

    let feet = render_feet(from, to, clock);
    let yaw = render_yaw(from, to, clock);
    let seat = lodestone_ecs::riding::player_seat_position(
        Vec3d::new(f64::from(feet.x), f64::from(feet.y), f64::from(feet.z)),
        yaw,
        kind.0.path(),
        facts.dimensions.height,
        seat_index,
    );
    Some(Vec3::new(seat.x as f32, seat.y as f32, seat.z as f32))
}

/// The currently-drawn body yaw, taking the shortest arc so a wrap across 360°
/// (e.g. 350°→10°) turns +20° rather than −340°.
fn render_yaw(from: &InterpFrom, to: &InterpTo, clock: &InterpClock) -> f32 {
    lerp_angle(from.yaw, to.yaw, alpha(clock))
}

/// The currently-drawn head yaw, shortest-arc like the body yaw.
fn render_head_yaw(from: &InterpFrom, to: &InterpTo, clock: &InterpClock) -> f32 {
    lerp_angle(from.head_yaw, to.head_yaw, alpha(clock))
}

/// The currently-drawn head pitch. Pitch is bounded to ±90° and never wraps, so
/// a plain linear ease is correct.
fn render_pitch(from: &InterpFrom, to: &InterpTo, clock: &InterpClock) -> f32 {
    from.pitch + (to.pitch - from.pitch) * alpha(clock)
}

/// The animation drive for this frame.
///
/// `partial_tick` is the fraction through the current 50 ms tick, used for the
/// walk cycle exactly as vanilla's `WalkAnimationState` interpolation. The head
/// yaw is clamped to the body (`Mob.clampHeadRotationToBody`) and then expressed
/// *relative* to it, because that is what `LivingEntityRenderer` feeds
/// `setupAnim` — passing the absolute value would spin every mob's head with its
/// body.
///
/// `swing_progress` is the entity's interpolated `attack_anim` for this frame —
/// `0.0` for an entity that has never swung, otherwise
/// [`lodestone_ecs::entity::AttackSwing::attack_anim_lerp`] resolved by the
/// caller through [`EntityIndex`] (see [`extract_entity_draws`]). This used to
/// be a hardcoded `0.0`: `ClientboundAnimatePacket` was decoded and folded
/// nowhere, so every other player and mob mined, hit and placed with a rigid
/// arm (issue #10 / `docs/arm-swing-animation.md`). The local player's own
/// swing is *not* this path — it is not a tracked network entity, and it goes
/// through `Sim::body_pose`/`Sim::hand_swing_progress` instead.
/// `aggressive` is `Mob.isAggressive()` — bit `0x04` of the mob-flags byte, folded
/// into [`MobState`] by `ingest::apply_entity_metadata` and resolved by the caller
/// through [`EntityIndex`] the same way `swing_progress` is (issue #379). It was a
/// hardcoded `false` here, which made the zombie arm lift in
/// `Skeleton::animate_zombie_arms` unreachable.
///
/// `crouching` is `Entity.isCrouching()` — the [`Pose`] component at metadata
/// index 6, **not** the shift-key bit of the shared-flags byte; see the `poses`
/// query in [`extract_entity_draws`] for why the two are not interchangeable.
/// It drives `Skeleton::pose`'s humanoid crouch branch, so a sneaking remote
/// player hunches exactly as the local self-avatar does.
///
/// `swim_amount` is `LivingEntity.getSwimAmount()`, interpolated for this
/// frame — the same [`SwimRamp`]-integrated value [`extract_entity_draws`]
/// already computes and carries on [`EntityDraw::swim_amount`] for the
/// whole-body prone rotation. This is the second, independent consumer: it
/// drives `Skeleton::pose`'s humanoid swim branch — the arm-over-arm stroke
/// and leg kick — which reads it off `AnimInput` rather than `EntityDraw`,
/// since that is the field `Skeleton::pose` actually takes.
fn render_anim(
    from: &InterpFrom,
    to: &InterpTo,
    clock: &InterpClock,
    walk: &WalkAnim,
    partial_tick: f32,
    swing_progress: f32,
    arm_pose: ArmPoseChoice,
    aggressive: bool,
    crouching: bool,
    is_passenger: bool,
    swim_amount: f32,
) -> AnimInput {
    let body = render_yaw(from, to, clock);
    let head = clamp_head_to_body(body, render_head_yaw(from, to, clock), MAX_HEAD_YAW);
    AnimInput {
        head_yaw_deg: wrap_degrees(head - body),
        head_pitch_deg: render_pitch(from, to, clock),
        limb_swing: walk.walk.position_lerp(partial_tick),
        limb_swing_amount: walk.walk.speed_lerp(partial_tick),
        attack_anim: swing_progress,
        age_ticks: clock.age,
        aggressive,
        arm_pose: arm_pose.pose,
        arm_pose_left_hand: arm_pose.left_hand,
        crouching,
        is_passenger,
        swim_amount,
    }
}

/// Vanilla's namespace. Matched explicitly rather than ignored: a resource pack
/// or mod item at `mypack:bow` is a *different* item and must not inherit the
/// bow's arm pose from its path alone.
const VANILLA: &str = "minecraft";
/// The `minecraft:bow` item path, matched by identity because the arm pose is a
/// per-item special case in vanilla too (`ItemUseAnimation.BOW`).
const BOW_PATH: &str = "bow";
/// The `minecraft:crossbow` item path.
const CROSSBOW_PATH: &str = "crossbow";
/// The `minecraft:air` item path — `ItemStack.isEmpty()`'s other half. A slot
/// holding air is an *empty* hand, so it must not raise an arm.
const AIR_PATH: &str = "air";

/// Vanilla's `CrossbowItem.getChargeDuration` with **no Quick Charge**:
/// `25 - 5 * level`, at level 0.
///
/// The enchantment level is not modelled. Reading it would mean resolving a
/// stack's `minecraft:enchantments` list against the enchantment registry, and
/// while [`lodestone_model::ItemComponents::enchantments`] does carry the list,
/// the render-side equipment set is narrowed to bare item ids
/// ([`RenderEquipment`]) long before it reaches here — an enchanted crossbow
/// therefore charges visually slower than it really does, and finishes its wind
/// animation late. Recorded rather than fixed because widening `RenderEquipment`
/// to full stacks is a larger change than the pose it would serve.
///
/// **Shared with the item *model* path** rather than restated: the same number
/// divides `minecraft:crossbow/pull`, which picks `item/crossbow_pulling_0/1/2`.
/// Two copies would let a crossbow's arms and its model disagree about how far
/// along the same wind is, on the same frame. An alias rather than a re-import so
/// the derivation stays documented at the place the arm pose reads it.
const CROSSBOW_CHARGE_TICKS: f32 = lodestone_render::CROSSBOW_CHARGE_TICKS;

/// Which arm pose an entity's arms take, and in which hand.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct ArmPoseChoice {
    pose: ArmPose,
    left_hand: bool,
}

/// Chooses the arm pose from the item in the used hand and how long it has been
/// used — vanilla's `AvatarRenderer.getArmPose` / `AbstractSkeletonRenderer.getArmPose`,
/// reduced to the poses [`ArmPose`] models (issue #57).
///
/// # Bow vs crossbow: two different triggers, and only one is the using-item bit
///
/// * **Bow** — `ItemUseAnimation.BOW`, gated purely on
///   `getUsedItemHand() == hand && getUseItemRemainingTicks() > 0`. Our
///   [`ItemUse`] flag is exactly that gate.
/// * **Crossbow charge** — `ItemUseAnimation.CROSSBOW`, same gate, plus the wind
///   fraction from the tick counter.
/// # The mob trigger is a *different flag*, and it is checked first
///
/// Vanilla selects an arm pose per **renderer**, and
/// `AbstractSkeletonRenderer.getArmPose` overrides the base one:
/// `isAggressive() && mainHandItem.is(BOW)` ⇒ `BOW_AND_ARROW`, else `super`. That
/// branch is what makes a skeleton shooting at you actually draw. It is **not**
/// the using-item bit: a skeleton's ranged attack goal calls
/// `performRangedAttack` and never enters the item-use state, so `item_use` is
/// forever `using: false` for it and #57's mechanism — correct for players and
/// remote players — reaches zero mobs (issue #379).
///
/// The override is keyed on the entity type by
/// [`mob_draws_bow_when_aggressive`], because it is genuinely per-renderer: an
/// aggressive *zombie* holding a bow gets no such pose in vanilla, and an
/// aggressive pillager's poses come from a different enum on a different model
/// class entirely.
///
/// * **Crossbow hold** — **not** an in-use pose at all. Vanilla checks
///   `!swinging && is(CROSSBOW) && CrossbowItem.isCharged(stack)`, where
///   `isCharged` reads the stack's `minecraft:charged_projectiles` component.
///   That component is **not modelled** by this build's item codec
///   ([`lodestone_model::ItemComponents`] has no field for it, and an
///   unrecognised component sets `has_unmodeled` and halts the patch decode), so
///   a charged crossbow is indistinguishable here from an empty one and this
///   function can never return [`ArmPose::CrossbowHold`]. The pose *math* is
///   implemented and tested in `lodestone-render`; what is missing is the wire
///   fact that selects it. Deliberately left as a gap rather than approximated:
///   guessing "charged" from anything else available would make every crossbow in
///   the world hold the shooting pose permanently, which is more wrong, more
///   often, than the resting pose it gets today.
///
/// # `ArmPose::Item` for a merely-held item, and why only an avatar gets it
///
/// Vanilla's final `return itemInHand.is(ItemTags.SPEARS) ? SPEAR : ITEM;` runs for
/// **any non-empty hand**, in use or not — but that line is in
/// `AvatarRenderer.getArmPose`, and **a humanoid mob never reaches it**.
/// `HumanoidMobRenderer.getArmPose`, the base every mob override delegates to, ends
/// `? SPEAR : EMPTY` instead. So a player holding a sword raises the arm and a zombie
/// holding the same sword does not, and [`renderer_is_avatar`] is what separates
/// them. An earlier reading of this file had it that *every* armed mob raises an arm
/// in vanilla; that was a transcription of the right method for the wrong renderer,
/// and acting on it would have posed every armed zombie, skeleton and armour stand.
///
/// Because the fallthrough is now avatar-only, it changes **no** mob silhouette, and
/// neither bow-pose pixel gate (both of which use a skeleton subject and a zombie
/// control) needed re-baselining.
fn arm_pose_for(
    type_path: &str,
    equipment: &[(EquipmentSlot, ResourceLocation)],
    item_use: Option<ItemUse>,
    aggressive: bool,
    main_arm_left: bool,
) -> ArmPoseChoice {
    // `AbstractSkeletonRenderer.getArmPose`'s override, ahead of the base
    // using-item rule exactly as vanilla's `? :` puts it ahead of `super`.
    //
    // `getMainArm() == arm` is vanilla's left-handed fork. The bow always sits
    // in the main *hand* (`main_hand_holds_bow` only ever looks at
    // `EquipmentSlot::MainHand`), so the physical arm that draws it is simply
    // `main_arm_left` — a left-handed skeleton draws with its left arm.
    if aggressive && mob_draws_bow_when_aggressive(type_path) && main_hand_holds_bow(equipment) {
        return ArmPoseChoice {
            pose: ArmPose::BowAndArrow,
            left_hand: main_arm_left,
        };
    }
    if let Some(choice) = in_use_arm_pose(equipment, item_use, main_arm_left) {
        return choice;
    }
    held_item_arm_pose(type_path, equipment, main_arm_left)
}

/// `AvatarRenderer.getArmPose`'s using-item `if` chain — the poses that need the
/// item to be *in use*, ahead of the merely-held fallthrough.
///
/// `None` means "no in-use pose applies", which is a different answer from
/// `Some(Empty)`: it lets [`held_item_arm_pose`] have its turn. Extracted so the
/// composition of the two halves has a name, because that seam is where the
/// interesting mistakes live — the in-use half alone was shipped once, and reading
/// it in isolation is what made "vanilla is wider" look like a one-line widening of
/// *this* function rather than a per-renderer question.
fn in_use_arm_pose(
    equipment: &[(EquipmentSlot, ResourceLocation)],
    item_use: Option<ItemUse>,
    main_arm_left: bool,
) -> Option<ArmPoseChoice> {
    let item_use = item_use?;
    if !item_use.using {
        return None;
    }
    let slot = if item_use.off_hand {
        EquipmentSlot::OffHand
    } else {
        EquipmentSlot::MainHand
    };
    // Using something we were never told about. Falling through rather than
    // guessing: equipment and metadata are separate packets and either can arrive
    // first.
    let (_, held) = equipment.iter().find(|(s, _)| *s == slot)?;
    if held.namespace() != VANILLA {
        return None;
    }
    let pose = match held.path() {
        BOW_PATH => ArmPose::BowAndArrow,
        CROSSBOW_PATH => ArmPose::CrossbowCharge {
            progress: item_use.ticks as f32 / CROSSBOW_CHARGE_TICKS,
        },
        // Vanilla's `ItemUseAnimation` chain, reduced. **For eating and drinking
        // this is not an approximation — it is what vanilla does.**
        // `ItemUseAnimation.EAT` and `DRINK` are deliberately absent from that
        // chain, so a consuming entity takes the plain held-item raise and the
        // whole distinctive eating motion lives in
        // `ItemInHandRenderer.applyEatTransform`, first person only.
        //
        // For the poses `ArmPose` still does not model — `BLOCK` (a raised shield),
        // `SPYGLASS`, `TOOT_HORN`, `BRUSH`, `THROW_TRIDENT`, `SPEAR` — this is a
        // *closer* wrong answer than `Empty`, not a right one: vanilla reaches each
        // of those before the fallthrough. `Item` at least puts the arm up, which is
        // the half those poses share; arms hanging at the sides is the reading that
        // looks like the feature is off.
        _ => ArmPose::Item,
    };
    Some(ArmPoseChoice {
        pose,
        // The *physical* arm, not the equipment slot: for a right-handed mob
        // (the common case) the off hand is the left arm and this is just
        // `item_use.off_hand`, but a left-handed mob's off hand is its right
        // arm — hence the XOR against `main_arm_left` rather than the bare
        // slot bit.
        left_hand: item_use.off_hand != main_arm_left,
    })
}

/// `AvatarRenderer.getArmPose`'s tail: any non-empty hand gets `ArmPose.ITEM`,
/// whether the item is in use or not.
///
/// **Avatar renderers only** — see [`renderer_is_avatar`] for the measurement. A
/// humanoid *mob* ends at `HumanoidMobRenderer.getArmPose`'s `EMPTY`, so its arms
/// hang, which is what this build already did and what vanilla does.
///
/// Vanilla poses each arm from its own hand and can raise **both** at once
/// (`getMainArm() == arm ? mainHandPose : offHandPose`). [`ArmPoseChoice`] carries
/// one pose and one hand, so the main hand wins when both are full; the off hand is
/// reached only when the main hand is empty, which is the case that would otherwise
/// pose the wrong arm.
fn held_item_arm_pose(
    type_path: &str,
    equipment: &[(EquipmentSlot, ResourceLocation)],
    main_arm_left: bool,
) -> ArmPoseChoice {
    if !renderer_is_avatar(type_path) {
        return ArmPoseChoice::default();
    }
    for (slot, off_hand) in [
        (EquipmentSlot::MainHand, false),
        (EquipmentSlot::OffHand, true),
    ] {
        if hand_is_occupied(equipment, slot) {
            return ArmPoseChoice {
                pose: ArmPose::Item,
                // Physical arm, same XOR as `in_use_arm_pose`.
                left_hand: off_hand != main_arm_left,
            };
        }
    }
    ArmPoseChoice::default()
}

/// `!itemInHand.isEmpty()` for one hand.
///
/// [`RenderEquipment`] only carries occupied slots — the narrowing drops an explicit
/// clear rather than keeping it as a present-but-empty entry — so presence is most of
/// the answer. `minecraft:air` is rejected as well: `ItemStack.isEmpty()` is
/// air-or-zero-count, and a server that sends air explicitly must not raise an arm.
fn hand_is_occupied(equipment: &[(EquipmentSlot, ResourceLocation)], slot: EquipmentSlot) -> bool {
    equipment
        .iter()
        .any(|(s, item)| *s == slot && !(item.namespace() == VANILLA && item.path() == AIR_PATH))
}

/// `mob.getMainHandItem().is(Items.BOW)` — the *item identity* half of the
/// skeleton override.
///
/// Split out so the "does the fixture's mob actually hold a bow in the equipment
/// the draw reads" question has one place to be true. That is the world species of
/// vacuous test: a gate whose skeleton has a bow in `OffHand`, or in no slot at
/// all, exercises none of this and still reads as a wiring failure.
fn main_hand_holds_bow(equipment: &[(EquipmentSlot, ResourceLocation)]) -> bool {
    equipment.iter().any(|(slot, item)| {
        *slot == EquipmentSlot::MainHand && item.namespace() == VANILLA && item.path() == BOW_PATH
    })
}

/// Wraps degrees into `(-180, 180]`, like `Mth.wrapDegrees`.
fn wrap_degrees(deg: f32) -> f32 {
    angle_diff(deg, 0.0)
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

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// `Update` / `FrameSet::Interpolate`: advance every ease clock and age by this
/// frame's [`FrameDelta`].
///
/// Runs **before** the 20 Hz tick loop, so a snapshot that resets `t` to 0 this
/// frame starts its new window from exactly the pose that was on screen.
pub fn advance_interp_clocks(delta: Res<FrameDelta>, mut clocks: Query<&mut InterpClock>) {
    for mut clock in &mut clocks {
        clock.t = (clock.t + delta.0).min(clock.window);
        clock.age += delta.0 * TICKS_PER_SECOND;
    }
}

/// `GameTick` / `TickSet::Animate`: one 20 Hz step of every entity's walk cycle.
///
/// Measures the per-tick travel off the *drawn* position, which is what vanilla
/// measures — see the module docs on why the interpolation gap is wrong by
/// [`INTERP_STEPS`]. It needs no separate "has it stopped?" rule: a mob that
/// stops stops moving its drawn position, so the distance goes to zero and the
/// amplitude decays on its own.
pub fn tick_walk_animation(
    mut tracks: Query<(&InterpFrom, &InterpTo, &InterpClock, &RenderScale, &mut WalkAnim)>,
) {
    for (from, to, clock, scale, mut walk) in &mut tracks {
        let now = render_feet(from, to, clock);
        let distance = (now - walk.last_feet).with_y(0.0).length();
        walk.last_feet = now;
        let limb_scale = if scale.0 < 1.0 {
            BABY_LIMB_SCALE
        } else {
            ADULT_LIMB_SCALE
        };
        walk.walk
            .update(walk_target_speed(distance), LIMB_SWING_SMOOTHING, limb_scale);
    }
}

/// `Extract` / `ExtractSet::Entities`: components → the plain [`EntityDraw`]
/// PODs `lodestone-render` consumes.
///
/// This is the boundary `docs/bevy-migration.md` §4.4 draws: the ECS side ends
/// here, and nothing downstream of it knows bevy exists.
pub fn extract_entity_draws(
    clock: Res<lodestone_ecs::FrameClock>,
    stacks: Res<ItemStacks>,
    // `AttackSwing` lives on the *ingest* entity (`lodestone_ecs::ingest::
    // apply_entity_animation` resolves `EntityAnimation` through `EntityIndex`),
    // not on the render entity this query's tuple is drawn from — `entity_id`
    // is the only key the two families share, so `index` is the bridge. See
    // `render_anim`'s doc for why this is not folded through `EntityFacts`
    // like every other field here.
    index: Res<EntityIndex>,
    swings: Query<&AttackSwing>,
    // `HurtTime` lives on the ingest entity too (`apply_entity_damaged` /
    // `apply_entity_hurt_animation` resolve it through the same `EntityIndex`),
    // so it is bridged the same way `AttackSwing` is rather than folded through
    // `EntityFacts` — see `EntityDraw::hurt`.
    hurts: Query<&HurtTime>,
    // `DeathTime` lives on the ingest entity too (`apply_entity_status` resolves
    // `EntityStatus`'s byte 3 through the same `EntityIndex`), bridged the same way
    // `HurtTime` is — and read beside it, because vanilla's red overlay is the
    // disjunction of the two. It is what turns a death packet into a mob toppling
    // onto its side; see `EntityDraw::death_time`.
    deaths: Query<&DeathTime>,
    // `ItemUse` lives on the ingest entity too (`apply_entity_item_use` resolves
    // the living-flags byte through the same `EntityIndex`), bridged the same way
    // `AttackSwing` and `HurtTime` are. It is what turns a metadata bit into a bow
    // draw — see `arm_pose_for` and issue #57.
    item_uses: Query<&ItemUse>,
    // `EntityFlags` lives on the ingest entity too
    // (`apply_entity_metadata` inserts it from the *shared*-flags byte —
    // index 0, not the living-flags byte `MobState`/`ItemUse` read — through
    // the same `EntityIndex`), bridged the same way. It is what turns bit
    // `0x01` into the mob-fire billboard — see `EntityDraw::on_fire` and
    // issue #434.
    flags: Query<&EntityFlags>,
    // `MobState` lives on the ingest entity too (`apply_entity_metadata` folds the
    // *mob* flags byte into it), bridged the same way. It is what turns
    // `Mob.isAggressive()` into a drawn bow and into a zombie's raised arms — see
    // `arm_pose_for` and issue #379.
    mob_states: Query<&MobState>,
    // `Pose` lives on the ingest entity too (`apply_entity_metadata` inserts it
    // from the pose accessor at index 6 — a *separate* field from the shared-flags
    // byte `EntityFlags` carries), bridged the same way. It was folded and read by
    // nothing: this is the query that turns it into a sneaking player's crouch.
    //
    // **Deliberately not `EntityFlags & 0x02`.** Vanilla's `isCrouching()` is
    // `hasPose(Pose.CROUCHING)` (`Entity.java`) and the shift-key bit is
    // `isShiftKeyDown()`/`isDiscrete()` (`:2691-2705`) — which is what the
    // *nametag* see-through gate below reads, correctly, because
    // `EntityRenderer.shouldShowName` really does ask `isDiscrete()`. Two
    // questions, two fields; a shift-key-down player whose standing box does not
    // fit is `SWIMMING`, not `CROUCHING`.
    poses: Query<&Pose>,
    // `FallingBlockState` lives on the ingest entity too
    // (`apply_falling_block_state` inserts it from the spawn packet's Object Data
    // field through the same `EntityIndex`), bridged the same way. It is what turns
    // one VarInt into a drawn block — see `EntityDraw::block_state`, and note it is
    // the *only* thing a client is ever told about which block is falling.
    falling_blocks: Query<&FallingBlockState>,
    // `ExperienceOrbValue` lives on the ingest entity too
    // (`lodestone_ecs::ingest::apply_entity_metadata` inserts it from index 8's
    // `INT`, gated on the adapter having established the entity is an orb),
    // bridged the same way `FallingBlockState` and `EntityFlags` above are — and
    // for the same structural reason: the component is on the *ingest* entity, not
    // on the render track this query's tuple is drawn from.
    //
    // Bridged rather than folded through `EntityFacts` into a fourteenth render
    // component, which is the route `RenderWool` takes. Both work; this one adds
    // nothing to `spawn_track`/`update_track` and nothing to the tuple above,
    // which is already at fourteen.
    orb_values: Query<&ExperienceOrbValue>,
    // The wire variant, on the ingest entity, bridged like `orb_values` above —
    // resolved to a texture sheet by `lodestone_render::entity_variant_sheet_for`.
    // See `EntityDraw::variant_sheet`.
    variants: Query<&lodestone_ecs::entity::Variant>,
    // `Tamed` lives on the ingest entity too (`lodestone_ecs::ingest::
    // apply_entity_metadata` folds `EntityMetadataUpdate::tamed` into it),
    // bridged the same way `variants` is. It is the last hop of issue #235's
    // chain: without it `variant_sheet` below has no source for the tame bit
    // and a tamed wolf draws the wild sheet forever.
    //
    // Paired with `vehicles` in one tuple parameter rather than two separate
    // ones: `bevy_ecs`'s `SystemParam` tuple impl tops out at 16 top-level
    // parameters, and this function was already at that ceiling before
    // `vehicles` needed to be added — nesting a tuple-of-`SystemParam` here
    // is itself one `SystemParam`, so it stays under the limit without
    // touching any of the other fifteen.
    //
    // `Vehicle` lives on the ingest entity too (`ingest::apply_entity_passengers`
    // folds it from `SET_PASSENGERS`'s reverse edge), bridged the same way
    // `tameds` is. Its mere *presence* is the sit-pose switch: a rider's own
    // network id never appears on its own entity, only the vehicle's it
    // names, so "is riding" is "does this component exist" for any tracked
    // (non-local) entity. See `AnimInput::is_passenger`.
    // `ArmorStandFlags` lives on the ingest entity too (`ingest::
    // apply_entity_metadata` folds `MetadataClass::ArmorStand`'s byte into
    // it), bridged the same way `tameds`/`vehicles` are — nested into this
    // tuple rather than added as a seventeenth top-level parameter, for the
    // same `SystemParam`-arity reason `vehicles` was.
    // `ItemFrameRotation` lives on the ingest entity too (`ingest::
    // apply_entity_metadata` folds index 10's `INT` into it, gated on the adapter
    // having established the entity is a frame), bridged the same way
    // `tameds`/`vehicles`/`armor_stands` are — and nested into this same tuple
    // rather than added as a seventeenth top-level parameter, for the identical
    // `SystemParam`-arity reason. Adding it at the top level really does fail to
    // compile, with an `in_set` "method not found" error a hundred lines away
    // from the parameter that caused it.
    (tameds, vehicles, armor_stands, item_frame_rotations): (
        Query<&lodestone_ecs::entity::Tamed>,
        Query<&lodestone_ecs::entity::Vehicle>,
        Query<&lodestone_ecs::entity::ArmorStandFlags>,
        Query<&ItemFrameRotation>,
    ),
    tracks: Query<(
        &MinecraftEntityId,
        &RenderKind,
        &RenderScale,
        &InterpFrom,
        &InterpTo,
        &InterpClock,
        &WalkAnim,
        &RenderEquipment,
        &RenderEquipmentDye,
        &RenderEquipmentTrim,
        &RenderWool,
        &RenderNameTag,
        &RenderPlayerSkin,
        // Nested into one tuple slot, not three top-level ones, for the same
        // `SystemParam`/`WorldQuery` tuple-arity reason `(tameds, vehicles,
        // armor_stands)` above is nested — this tuple was already at fourteen
        // top-level items before `CapeLag` needed to join it.
        (
            // `Option`, not `&CreeperFuse` bare: present only on creepers,
            // same "absence is the switch" shape `ItemPhysics` uses elsewhere
            // in this module. Every non-creeper entity matches `None` here at
            // zero cost.
            Option<&CreeperFuse>,
            // Bare, not `Option`: `spawn_track` inserts this on every track
            // entity unconditionally — see [`SwimRamp`]'s own doc for why it
            // is not gated by `RenderKind` the way `CreeperFuse` is.
            &SwimRamp,
            // Bare too, same reason and the same unconditional
            // `spawn_track` insert — see [`CapeLag`]'s own doc.
            &CapeLag,
        ),
    )>,
    mut out: ResMut<ExtractedDraws>,
) {
    // The one accumulator's residual, published by `FrameClock::end_frame` before
    // `Extract` runs. This used to be `TickAccum`, the interpolator `World`'s own
    // second accumulator; it is now the *same* number the camera interpolates the
    // player with, which is the point of §4.1(c).
    let partial_tick = clock.interp_alpha.clamp(0.0, 1.0);
    out.0.clear();
    for (
        id,
        kind,
        scale,
        from,
        to,
        clock,
        walk,
        equipment,
        equipment_dye,
        equipment_trim,
        wool,
        name_tag,
        player_skin,
        (fuse, swim, cape_lag),
    ) in &tracks
    {
        // One lookup, not two: `item` and `count` both come from the same
        // recorded stack, and a drop with no stack yet must not manufacture a
        // count out of nowhere.
        //
        // **Not narrowed to `ITEM_ENTITY_TYPE_PATH`, and that narrowing is what
        // made three consumers dead code.** `ItemStacks` is only ever written
        // for an entity whose server metadata actually carried the `ITEM_STACK`
        // serializer, so keying it on the entity *type* as well added nothing
        // and silently answered `None` for every other claimant of that same
        // serializer: an item frame's contents (`ItemFrame.DATA_ITEM`), a
        // framed filled map, and a thrown projectile's real stack. Each of
        // those consumers is written, tested and reachable — and each is
        // guarded by its own entity-type check at the draw site, so widening
        // here cannot make anything draw twice. Every gate for them built its
        // own `EntityDraw` with `item: Some(..)` by hand and so could not see
        // the producer refusing to supply one; `live_framed_item_wire.rs` is
        // the gate that obtains this value the way production does.
        let stack = stacks.0.get(&id.0);
        // `0.0` for an id with no ingest entity (shouldn't happen — a render
        // track only exists once the entity has been spawned) or one that has
        // never swung (`AttackSwing` absent, like `HurtTime`).
        let swing_progress = index
            .get(id.0)
            .and_then(|entity| swings.get(entity).ok())
            .map_or(0.0, |swing| swing.attack_anim_lerp(partial_tick));
        // `deathTime + partialTicks` while dying, `0.0` while alive — vanilla's
        // `LivingEntityRenderer.extractRenderState`:
        // `state.deathTime = entity.deathTime > 0 ? entity.deathTime + partialTicks : 0.0F`.
        //
        // The ternary is not decoration: `DeathTime` is inserted at **zero** on the
        // tick death is announced (see its own doc), and a bare `+ partial_tick`
        // would make that first tick report a fractional death time, starting the
        // fall-over — and the red overlay's `deathTime > 0` half — mid-frame instead
        // of on the tick boundary. Absent `DeathTime` is "alive", so this reads
        // `0.0` for every living entity at no cost.
        let death_time = index
            .get(id.0)
            .and_then(|entity| deaths.get(entity).ok())
            .map_or(0.0, |death| {
                if death.0 > 0 {
                    death.0 as f32 + partial_tick
                } else {
                    0.0
                }
            });
        // `hurtTime > 0 || deathTime > 0`, vanilla's `hasRedOverlay` gate in full.
        // `false` for an entity that has never been hit (`HurtTime` absent, like
        // `AttackSwing`) — and also for one whose countdown has aged out, since
        // `tick_hurt_time` leaves the component in place at zero rather than
        // removing it.
        //
        // The `deathTime` half is what this field's doc used to name as its one
        // known divergence: on `hurtTime` alone the overlay ends ten ticks after the
        // killing blow, so a mob went red, turned its normal colour again, and only
        // *then* fell over. The disjunction is why vanilla's tint carries all the
        // way through the fall-over — the two counters run in opposite directions
        // and overlap by design.
        let hurt = index
            .get(id.0)
            .and_then(|entity| hurts.get(entity).ok())
            .is_some_and(|hurt| hurt.0 > 0)
            || death_time > 0.0;
        // The using-item state behind the bow/crossbow arm pose. `None` for an
        // entity that has never reported the byte (`ItemUse` absent, like
        // `AttackSwing`), which `arm_pose_for` reads as "not using anything".
        // `Mob.isAggressive()`. `false` for an entity that has never reported the
        // mob-flags byte (`MobState` absent) — which includes every non-`Mob`
        // entity permanently, because the adapter withholds index 15 for those.
        let aggressive = index
            .get(id.0)
            .and_then(|entity| mob_states.get(entity).ok())
            .is_some_and(|state| state.aggressive);
        // `Mob.getMainArm() == LEFT` — same bridge as `aggressive` above, off
        // the same `MobState` component, `false` for the same absent case.
        let main_arm_left = index
            .get(id.0)
            .and_then(|entity| mob_states.get(entity).ok())
            .is_some_and(|state| state.left_handed);
        // One lookup, two consumers: the arm pose below and `EntityDraw::item_use`,
        // which the held-item pass resolves the item's own definition tree against.
        // Reading it twice would let the two disagree about the same tick.
        let item_use = index
            .get(id.0)
            .and_then(|entity| item_uses.get(entity).ok())
            .map(|item_use| *item_use);
        let arm_pose = arm_pose_for(&kind.0, &equipment.0, item_use, aggressive, main_arm_left);
        // `Entity.isCrouching()`. `false` for an entity that has never reported
        // the pose accessor (`Pose` absent) — which is every entity that has
        // never left `STANDING`, since the server only sends metadata that
        // differs from the default.
        let crouching = index
            .get(id.0)
            .and_then(|entity| poses.get(entity).ok())
            .is_some_and(|pose| pose.0 == lodestone_model::EntityPose::Crouching);
        // `Entity.isPassenger()`. `false` for an entity that has never been
        // named as a rider by `SET_PASSENGERS` (`Vehicle` absent) — see
        // `vehicles`'s own doc above. This is the local-player-*excluded* half
        // of the sit pose: the local player never has an ingest entity of its
        // own to carry `Vehicle`, and gets the same bit from
        // `lodestone_ecs::session::Riding` instead — see `Sim::body_anim`.
        let is_passenger = index
            .get(id.0)
            .and_then(|entity| vehicles.get(entity).ok())
            .is_some();
        // `0.0` (and hence a bit-identical `pose_swelling` to `pose`, per that
        // function's own doc) for every non-creeper — `fuse` is `None` — and
        // for a creeper whose fuse has never moved off idle. Vanilla's own
        // `Creeper.getSwelling`: `lerp(partialTick, oldSwell, swell) /
        // (maxSwell - 2)`, `maxSwell` fixed at 30 client-side (see
        // `CREEPER_MAX_SWELL_TICKS`'s doc).
        let creeper_swelling = fuse.map_or(0.0, |fuse| {
            let old = fuse.old_swell as f32;
            let new = fuse.swell as f32;
            (old + (new - old) * partial_tick) / (CREEPER_MAX_SWELL_TICKS as f32 - 2.0)
        });
        // `Mth.lerp(partialTick, swimAmountO, swimAmount)` — see [`SwimRamp`]
        // for why this is integrated here rather than read off the wire.
        let swim_amount = swim.old + (swim.current - swim.old) * partial_tick;
        // `AvatarRenderer.extractCapeState`, given this frame's interpolated
        // lagged cloak position (against the *drawn* feet, exactly as
        // vanilla's own `Mth.lerp(partialTicks, entity.xo, entity.getX())`
        // resolves against the same partial tick every other interpolated
        // field here does) and this frame's body yaw. `walk.walk.position_lerp`
        // stands in for `ClientAvatarState.getInterpolatedWalkDistance` — a
        // different accumulator in vanilla, but the same shape (a
        // monotonic walk-cycle distance that drives the flap's footstep-synced
        // wobble), and the only one already tracked on this component.
        let cape_lag_pos = Vec3::new(
            cape_lag.cloak_o.x + (cape_lag.cloak.x - cape_lag.cloak_o.x) * partial_tick,
            cape_lag.cloak_o.y + (cape_lag.cloak.y - cape_lag.cloak_o.y) * partial_tick,
            cape_lag.cloak_o.z + (cape_lag.cloak.z - cape_lag.cloak_o.z) * partial_tick,
        );
        let cape_bob = cape_lag.bob_o + (cape_lag.bob - cape_lag.bob_o) * partial_tick;
        let cape_sway_value = cape_sway(
            cape_lag_pos - render_feet(from, to, clock),
            render_yaw(from, to, clock),
            cape_bob,
            walk.walk.position_lerp(partial_tick),
        );
        // Bit `0x01` of the shared-flags byte. `false` for an entity that has
        // never reported the byte at all (`EntityFlags` absent, like
        // `HurtTime`/`AttackSwing`) — see `EntityDraw::on_fire`.
        let on_fire = index
            .get(id.0)
            .and_then(|entity| flags.get(entity).ok())
            .is_some_and(|flags| flags.0 & 0x01 != 0);
        // Bit `0x20` of the same shared-flags byte `on_fire` reads bit `0x01`
        // of. `false` for an entity that has never reported the byte, exactly
        // like `on_fire` — see `EntityDraw::invisible`.
        let invisible = index
            .get(id.0)
            .and_then(|entity| flags.get(entity).ok())
            .is_some_and(|flags| flags.0 & 0x20 != 0);
        // An armour stand's own client-flags byte, bridged off the ingest
        // entity through `index` exactly as `on_fire`/`invisible` above are.
        // `None` for every entity that is not an `ArmorStand` (the adapter
        // withholds the byte for those) and for one that has never reported
        // it yet — see `EntityDraw::armor_stand`.
        let armor_stand = index
            .get(id.0)
            .and_then(|entity| armor_stands.get(entity).ok())
            .copied();
        // The imitated block state of a falling block. Bridged off the ingest
        // entity through `index` exactly as `on_fire` above and `hurt` below are,
        // because `lodestone_ecs::ingest::apply_falling_block_state` inserts the
        // component *there* and not on the render entity this query is drawn from.
        // `None` for every entity that is not a falling block, which is the switch
        // the moving-block pass keys on.
        let block_state = index
            .get(id.0)
            .and_then(|entity| falling_blocks.get(entity).ok())
            .map(|state| state.0);
        // An experience orb's XP value, bridged off the ingest entity like
        // `block_state` above. `None` for every entity that is not an orb — the
        // adapter withholds index 8's `INT` for those — which is the switch
        // `prepare_orbs` keys on. An orb whose value has not arrived yet is still
        // drawn, at sprite cell 0; see `EntityDraw::experience_orb_value`.
        let experience_orb_value = if kind.0.as_ref() == EXPERIENCE_ORB_TYPE_PATH {
            Some(
                index
                    .get(id.0)
                    .and_then(|entity| orb_values.get(entity).ok())
                    .map_or(0, |value| value.0),
            )
        } else {
            None
        };
        // An item frame's in-plane rotation, bridged off the ingest entity like
        // `block_state` and `experience_orb_value` above. `0` — vanilla's own
        // accessor default — for every entity that is not a frame and for a frame
        // that has not reported one, because unlike a block state there is no
        // "absent" case a consumer would draw differently.
        let item_frame_rotation = index
            .get(id.0)
            .and_then(|entity| item_frame_rotations.get(entity).ok())
            .map_or(0, |rotation| rotation.0);
        // The variant sheet, bridged off the ingest entity's `Variant` exactly as
        // `block_state` and `experience_orb_value` above are, and for their reason:
        // `lodestone_ecs::ingest::apply_entity_metadata` inserts `Variant` *there*,
        // not on the render track this query is drawn from. Bridged rather than
        // narrowed into a fifteenth render component — the same choice
        // `experience_orb_value` documents one block up.
        //
        // `kind.0` and not `model_type_path()`: the variant axis belongs to the
        // *species* corpus entry, and the only case where the two differ is a
        // player's slim rig, which has no variant axis at all.
        //
        // `tamed` is bridged off the ingest entity exactly like `variant` above,
        // and defaults to `false` (the wild sheet) for an entity whose `Tamed`
        // has never been reported — the honest default for anything that is not
        // a wolf/cat/parrot/ocelot, and for one of those that really is wild.
        let tamed = index
            .get(id.0)
            .and_then(|entity| tameds.get(entity).ok())
            .is_some_and(|tamed| tamed.0);
        // A player's built-in identity sheet takes this channel first. The two
        // can never contend — a player has no `Variant`, and nothing with a
        // variant axis carries a `player_skin` — so the `or_else` is an
        // ordering statement rather than a precedence rule, and it keeps one
        // field meaning "the sheet this entity binds instead of its model's".
        //
        // This is what makes `DefaultPlayerSkin`'s hash pick visible at all.
        // Until it existed the pick's `.model` chose the rig and its `.texture`
        // was dropped, so all eighteen identities drew the pack's two plain
        // sheets: every skinless player was Steve or Alex.
        let variant_sheet = player_skin
            .0
            .as_ref()
            .map(|skin| skin.default_sheet)
            .or_else(|| {
                index.get(id.0).and_then(|entity| variants.get(entity).ok()).and_then(|variant| {
                    lodestone_render::entity_variant_sheet_for(&kind.0, &variant.0, tamed)
                })
            });
        out.0.push(EntityDraw {
            id: id.0,
            type_path: Arc::clone(&kind.0),
            variant_sheet,
            item: stack.map(|s| s.id.clone()),
            count: stack.map_or(1, |s| s.count),
            foil: stack.is_some_and(|s| s.foil),
            item_dyed_color: stack.and_then(|s| s.dyed_color),
            item_potion_color: stack.and_then(|s| s.potion_color),
            equipment: equipment.0.clone(),
            equipment_dye: equipment_dye.0.clone(),
            equipment_trim: equipment_trim.0.clone(),
            wool: wool.0,
            block_state,
            item_frame_rotation,
            feet: render_feet(from, to, clock),
            yaw: render_yaw(from, to, clock),
            head_yaw: render_head_yaw(from, to, clock),
            pitch: render_pitch(from, to, clock),
            scale: scale.0,
            anim: render_anim(
                from,
                to,
                clock,
                walk,
                partial_tick,
                swing_progress,
                arm_pose,
                aggressive,
                crouching,
                is_passenger,
                swim_amount,
            ),
            name_tag: name_tag.0.clone(),
            hurt,
            death_time,
            item_use,
            main_arm_left,
            creeper_swelling,
            swim_amount,
            on_fire,
            invisible,
            armor_stand,
            player_skin: player_skin.0.clone(),
            experience_orb_value,
            cape_sway: cape_sway_value,
        });
    }
}

/// `GameTick` / `TickSet::Physics`: one 20 Hz step of every dropped item's own
/// ballistic physics, re-anchoring each one's render ease onto the freshly
/// simulated point.
///
/// # Why this took until now to become a system
///
/// A `bevy_ecs` system reads its inputs from `Resource`s, and a `Resource`
/// must be `'static`. Before
/// [`lodestone_ecs::player::CollisionSource`] existed, the collision geometry
/// reached this function as a borrowed `&dyn CollisionView` whose owner was a
/// local in `Sim::update_entities` (`WorldCollision::new(&self.world)` borrows
/// the chunk world outright) — there was no safe way to put that borrow in a
/// resource, and the workspace denies `unsafe_code`.
///
/// `CollisionSource` inverts it: the *trait object* is `'static` because an
/// implementor owns whatever it borrows from, and only the `&dyn
/// CollisionView` handed to [`CollisionSource::with_view`]'s callback is
/// short-lived. [`EntityInterpolator::update_with_view`] inserts a
/// [`PlayerCollision`] (holding that `Arc<dyn CollisionSource>` in its `View`
/// variant) as a resource before running the tick loop, which is what makes
/// this reachable from `GameTick` at all.
///
/// The absence of [`ItemPhysics`] is still the switch that keeps every other
/// entity type on the pure position ease — the query below only ever matches
/// entities that have all four components, which [`spawn_track`] inserts
/// atomically.
pub fn tick_item_physics(
    collision: Res<ItemCollision>,
    profile: Res<Profile>,
    mut items: Query<(&mut ItemPhysics, &mut InterpFrom, &mut InterpTo, &mut InterpClock)>,
) {
    // `NoWorld`/`Pending` mean there is nothing to collide against yet — leave
    // every item's simulation exactly where it was rather than free-falling it
    // through geometry we cannot query.
    let PlayerCollision::View(source) = &collision.0 else {
        return;
    };
    let profile = &profile.0;
    source.with_view(&mut |view| {
        for (mut physics, mut from, mut to, mut clock) in &mut items {
            // Paused while the last *server* report says the item is resting;
            // the floor within a tick is real collision, not a frozen flag.
            if physics.grounded {
                continue;
            }
            step_item_physics(&mut physics.sim, view, profile);
            let simulated = to_glam_vec3(physics.sim.position);

            // Re-anchor exactly like a fresh authoritative snapshot would: ease
            // from wherever this frame is currently drawn toward the freshly
            // simulated point, so the simulation reads as continuous motion
            // rather than a series of per-tick snaps.
            let drawn = render_feet(&from, &to, &clock);
            from.feet = drawn;
            to.feet = simulated;
            clock.t = 0.0;
        }
    });
}

/// Seeds a fresh [`ItemPhysics`] from an item entity's first-seen (or freshly
/// re-anchored) snapshot. A missing velocity seeds zero — gravity still applies
/// to it, it just has nothing to arc with, which is exactly the discriminating
/// behaviour the hermetic tests below pin.
fn new_item_physics(snap: &EntityFacts) -> ItemPhysics {
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

/// The client-simulated body yaw for a **player** entity — vanilla's
/// `LivingEntity.yBodyRot`, run locally because nothing ever sends it.
///
/// A player's own `Rotation`/`HeadYaw` arrive over the wire **equal**:
/// `ServerEntity.sendChanges` broadcasts `Mth.packDegrees(entity.getYRot())`
/// as the move/rotation packet's angle and `Mth.packDegrees(entity.getYHeadRot())`
/// as the head packet's, and `Player.aiStep` forces `this.yHeadRot =
/// this.getYRot()` every tick — a player has no second, independently-aimed
/// value the way a `Mob`'s `LookControl` gives its head. So feeding the
/// reported [`Rotation`] yaw straight into [`EntityFacts::yaw`], which is
/// exactly right for a mob (whose body and AI-aimed head genuinely diverge
/// on the wire already), makes a *player's* body and head numerically
/// identical forever — the "turns as one rigid block, head never moves"
/// report this component exists to fix.
///
/// Real vanilla clients never receive a body yaw for another player either:
/// every receiving client's own `RemotePlayer` puppet runs
/// `LivingEntity.tick()`'s generic `tickHeadTurn` lag **locally**, deriving
/// its rendered body yaw from the received look yaw and that puppet's own
/// per-tick movement. [`tick_remote_body_yaw`] is that same simulation,
/// reusing [`crate::sim::step::body_yaw_target`]/[`crate::sim::step::tick_head_turn`]
/// — the identical port `sim/step.rs` already wrote for the local
/// third-person body — so the rule has one implementation, not two.
///
/// Lives on the **ingest** entity, beside `Rotation`/`HeadYaw`/`Position`,
/// not the render track: [`resolve_entity_facts`] reads it directly the same
/// way it reads those.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct BodyYawState {
    /// Vanilla's `yBodyRot`.
    yaw: f32,
    /// This entity's `Position` as of last tick — vanilla's `xo`/`zo`, the
    /// reference [`crate::sim::step::body_yaw_target`]'s `(dx, dz)` is
    /// measured against.
    last_feet: lodestone_model::Vec3,
}

/// `GameTick`/`TickSet::Animate`: advances [`BodyYawState`] for every tracked
/// **player** entity, once per tick — the client-side half of
/// `LivingEntity.tickHeadTurn` real vanilla runs for every other player's
/// puppet on every receiving client. See [`BodyYawState`]'s doc for why a
/// player needs this simulation and a mob (whose `Rotation` is already an
/// independent, AI-driven body yaw) must not get it — gated here on
/// `EntityKind::path` `== "player"`, exactly the check `resolve_entity_facts`
/// already uses for `is_player`.
///
/// Two passes over disjoint archetypes (`Without<BodyYawState>` for the
/// lazy-insert half) rather than one `Option<&mut BodyYawState>` query,
/// because a fresh player needs [`Commands`] to gain the component at all —
/// initialised to its own reported yaw, matching vanilla's spawn-time
/// `this.yBodyRot = this.getYRot()`, so a fresh join starts rigid for
/// exactly the one tick nothing has told it otherwise yet, not eased in from
/// a guessed value.
///
/// `attacking` reads [`AttackSwing::attack_anim`] the same tick
/// `lodestone_ecs::ingest::tick_entity_swing` ages it — one tick "behind"
/// vanilla's own ordering, the same way `sim/step.rs`'s identical call site
/// already documents itself as being, for the identical reason.
///
/// The `50.0` `max_head_rotation` is vanilla's un-narrowed
/// `LivingEntity.getMaxHeadRotationRelativeToBody()`. `Player`'s own `15.0`
/// override while blocking with a shield is **not** modelled here: it needs
/// a decoded remote block/use-item state this crate does not carry yet, so a
/// blocking remote player's body currently drags at the wider, un-narrowed
/// angle rather than vanilla's tighter one — a narrower gap than the "rigid
/// block" bug this fixes, and recorded rather than silently assumed away.
pub fn tick_remote_body_yaw(
    mut existing: Query<(
        &lodestone_ecs::entity::EntityKind,
        &lodestone_ecs::entity::Position,
        &lodestone_ecs::entity::Rotation,
        Option<&AttackSwing>,
        &mut BodyYawState,
    )>,
    missing: Query<
        (
            Entity,
            &lodestone_ecs::entity::EntityKind,
            &lodestone_ecs::entity::Position,
            &lodestone_ecs::entity::Rotation,
        ),
        Without<BodyYawState>,
    >,
    mut commands: Commands,
) {
    const MAX_HEAD_ROTATION_DEG: f32 = 50.0;
    for (kind, position, rotation, swing, mut state) in &mut existing {
        if kind.0.path() != "player" {
            continue;
        }
        let dx = position.0.x - state.last_feet.x;
        let dz = position.0.z - state.last_feet.z;
        let attacking = swing.is_some_and(|swing| swing.attack_anim > 0.0);
        let target =
            crate::sim::step::body_yaw_target(state.yaw, rotation.0.yaw, dx, dz, attacking);
        state.yaw = crate::sim::step::tick_head_turn(
            state.yaw,
            rotation.0.yaw,
            target,
            MAX_HEAD_ROTATION_DEG,
        );
        state.last_feet = position.0;
    }
    for (entity, kind, position, rotation) in &missing {
        if kind.0.path() != "player" {
            continue;
        }
        commands.entity(entity).insert(BodyYawState {
            yaw: rotation.0.yaw,
            last_feet: position.0,
        });
    }
}

/// Resolves the [`EntityFacts`] a live ingest entity carries, exactly the
/// narrowing `net::entity_snapshot` used to do against an
/// [`EntityView`](lodestone_client::EntityView) — now read straight off
/// [`lodestone_ecs::entity`]'s components, since [`fold_entities`] already
/// holds this same `World`'s write guard. Mirrors
/// `lodestone_client::state::entity_view`'s component reads field for field;
/// see that function if the two ever need to be compared.
///
/// Returns `None` when the entity is missing the four components every
/// networked entity carries ([`MinecraftEntityId`] is read by the caller,
/// which is why it is not repeated here) — [`EntityKind`], [`Position`],
/// [`Rotation`], [`HeadYaw`] — the same defensive shape
/// `lodestone_client::state::entity_view` uses, in case a caller ever hands
/// this a non-networked entity.
/// Whether `name` should draw no nametag at all — an actually-empty string,
/// or the literal text `"<empty>"`.
///
/// The second case is not a hypothetical: `"<empty>"` is Mojang's own
/// `toString()` convention for "this value is absent" — `HashedStack`,
/// `SlotDisplay` and `CommandResultCallback` (`.cache/mc/26.2/…`) each use
/// this *exact* string for their own no-value case, so a plugin (a common
/// source of `type=player` NPC entities, which need a registered tab-list
/// profile to carry a skin) that serialises an "absent name" sentinel the
/// same way Mojang's own internals debug-print one produces this precise
/// text over the wire — indistinguishable from a real player deliberately
/// named `<empty>` only by convention, not by any wire signal. Nothing in
/// this crate, `lodestone-model` or `lodestone-game` ever constructs this
/// string — grepping the whole tree for it finds only those Java classes'
/// own `toString()` bodies — so a name that arrives already reading exactly
/// this is not this client's own placeholder leaking through; it is the
/// server's "no name" state read back off the wire as if it were content.
#[must_use]
fn is_blank_name_tag(name: &str) -> bool {
    name.is_empty() || name == "<empty>"
}

fn resolve_entity_facts(
    id: i32,
    entity: bevy_ecs::world::EntityRef<'_>,
    tab_list: &lodestone_game::tablist::TabList,
) -> Option<EntityFacts> {
    use lodestone_ecs::entity::{
        Baby, CreeperSwellDir, CustomName, CustomNameVisible, DisplayItem, EntityFlags,
        EntityKind, EntityUuid, Equipment, HeadYaw, OnGround, Position, Rotation, Variant,
        Velocity,
    };

    let type_key = entity.get::<EntityKind>()?.0.clone();
    let position = entity.get::<Position>()?.0;
    let rotation = entity.get::<Rotation>()?.0;
    let head_yaw = entity.get::<HeadYaw>()?.0;
    // The reported `Rotation::yaw` is the right *body* yaw for a mob (its
    // body and AI-aimed head already diverge on the wire), but for a player
    // it is the same number as `head_yaw` above — see [`BodyYawState`]'s doc.
    // `tick_remote_body_yaw` (`GameTick`/`TickSet::Animate`) maintains the
    // locally-lagged alternative on this same ingest entity; absent means
    // either a non-player or the very first fold before that system has run
    // a tick yet, both of which fall back to the raw reported yaw exactly as
    // before this component existed.
    let body_yaw = entity
        .get::<BodyYawState>()
        .map_or(rotation.yaw, |state| state.yaw);

    let scale = if entity.get::<Baby>().is_some_and(|baby| baby.0) {
        0.5
    } else if entity
        .get::<lodestone_ecs::entity::ArmorStandFlags>()
        .is_some_and(|flags| flags.small)
    {
        // `ArmorStand.isSmall()` (`ArmorStandRenderer.submit`) bakes and draws
        // an entirely separate, smaller model rather than scaling the big one
        // — this renderer has no second bake, so a uniform half-scale stands
        // in for it, the same approximation already made for a baby mob two
        // lines up.
        0.5
    } else {
        1.0
    };

    // Borrowed, ahead of the by-value `item` match below, for the same reason
    // `net::entity_snapshot` read `count` first: it only exists on the wire's
    // `ItemStack`, which the match consumes converting the key. `1` is the
    // neutral default for every case with no stack to count.
    let display_item = entity
        .get::<DisplayItem>()
        .map_or(Reported::Unreported, |item| Reported::Reported(item.0.clone()));
    let count = match &display_item {
        Reported::Reported(Some(stack)) => stack.count,
        _ => 1,
    };
    // The glint gate for a dropped stack. `lodestone_render::glint::has_foil_for_item`
    // is the single owner of what foil means (the HUD's own
    // `item_icon::stack_has_foil` bridges the *other* stack type to the same
    // predicate); nothing here re-spells it.
    // Keyed on the item id as well as the components, because seven vanilla
    // items bake `ENCHANTMENT_GLINT_OVERRIDE` into their prototype and glint
    // with no enchantments at all — an enchanted book is the one a player
    // notices. Reading only the components answers `false` for every one of
    // them, which is what left a dropped or held enchanted book flat.
    let foil = match &display_item {
        Reported::Reported(Some(stack)) => lodestone_render::glint::has_foil_for_stack(
            &stack.item.to_string(),
            &stack.components,
        ),
        _ => false,
    };
    // Borrowed for the identical reason `count`/`foil` are: the wire's
    // `ItemComponents` is what `item_tint::resolve` needs, and the match below
    // consumes `display_item` converting the key to a bare id.
    let item_dyed_color = match &display_item {
        Reported::Reported(Some(stack)) => stack.components.dyed_color,
        _ => None,
    };
    let item_potion_color = match &display_item {
        Reported::Reported(Some(stack)) => stack.components.potion_color,
        _ => None,
    };
    // A failed conversion must collapse to `Unreported` ("nothing reported"),
    // never to `Reported(None)`, which downstream reads as the server
    // clearing the stack.
    let item = match display_item {
        Reported::Unreported => Reported::Unreported,
        Reported::Reported(None) => Reported::Reported(None),
        Reported::Reported(Some(stack)) => {
            match ResourceLocation::new(stack.item.namespace(), stack.item.path()) {
                Ok(id) => Reported::Reported(Some(id)),
                Err(_) => Reported::Unreported,
            }
        }
    };

    // `Equipment` is the *accumulated* per-slot state (`apply_entity_equipment`
    // merges each update into it and never clears), so every fold carries the
    // complete current set and the consumer can replace wholesale. Nesting is
    // preserved exactly: a slot **absent** is "never mentioned", present with
    // `None` is an explicit "this slot is empty" — collapsing the two would
    // make an armourless mob indistinguishable from one whose armour the
    // server confirmed gone.
    let raw_equipment = entity
        .get::<Equipment>()
        .map(|equipment| equipment.0.clone())
        .unwrap_or_default();
    // A key that fails `ResourceLocation` validation drops the whole *entry*
    // rather than degrading to `Some(slot, None)` — same rule as `item`
    // above: a malformed id must read as "not reported", never as the server
    // clearing the slot.
    let equipment = raw_equipment
        .iter()
        .filter_map(|eq| match &eq.item {
            None => Some((eq.slot, None)),
            Some(stack) => ResourceLocation::new(stack.item.namespace(), stack.item.path())
                .ok()
                .map(|id| (eq.slot, Some(id))),
        })
        .collect();
    // Narrowed the same way `equipment` is: a slot only carries a dye if its
    // item is present *and* its id validates. Emitting a dye for a slot
    // `equipment` dropped would describe a tint on an item the renderer was
    // never told about.
    let equipment_dye = raw_equipment
        .iter()
        .filter_map(|eq| {
            let stack = eq.item.as_ref()?;
            ResourceLocation::new(stack.item.namespace(), stack.item.path()).ok()?;
            Some((eq.slot, stack.components.dyed_color?))
        })
        .collect();
    // `minecraft:trim`, narrowed identically (issue #17). Kept out of
    // `equipment_dye`'s tuple deliberately: an item can carry both, and the two
    // reach the GPU by different routes — dye as an instance tint, trim as its own
    // texture and therefore its own batch.
    let equipment_trim = raw_equipment
        .iter()
        .filter_map(|eq| {
            let stack = eq.item.as_ref()?;
            ResourceLocation::new(stack.item.namespace(), stack.item.path()).ok()?;
            Some((eq.slot, stack.components.trim.clone()?))
        })
        .collect();

    // Nametag resolution (issue #100). Two entirely different rules, per the
    // real 26.2 client — see the historical `net::entity_snapshot` doc this
    // was ported from for the exact vanilla predicates (jar file:line):
    // a player's tag is always its tab-list display name, unconditionally;
    // every other entity's tag is its custom name, gated on
    // `CUSTOM_NAME_VISIBLE`.
    let is_player = type_key.path() == "player";
    let uuid = entity.get::<EntityUuid>().map(|uuid| uuid.0);
    let flags = entity.get::<EntityFlags>().map(|f| f.0);
    let custom_name = entity
        .get::<CustomName>()
        .map_or(Reported::Unreported, |name| Reported::Reported(name.0.clone()));
    let custom_name_visible = entity.get::<CustomNameVisible>().map(|visible| visible.0);
    // Resolved as a styled `Text`, not a flattened plain string — a player's
    // tab-list `effective_name()` and a mob's `custom_name` metadata both
    // carry colour/bold/italic/underline/strikethrough now (the fix this
    // block exists for: nametags used to lose all of that at this exact
    // resolution point via `to_plain_string`/`plain_text_from_nbt_component`).
    let name_tag: Option<Text> = if is_player {
        uuid.and_then(|id| match tab_list.get(&id) {
            Some(entry) => {
                let styled = entry.effective_name();
                let plain = styled.to_plain_string();
                if is_blank_name_tag(&plain) {
                    None
                } else {
                    // Same fallback the skin lookup two fields down already
                    // has: remember every real resolution against this uuid
                    // so a later frame whose tab-list entry has vanished (a
                    // `player_info_remove`, or a plugin NPC that adds then
                    // removes its entry while the entity stays spawned) can
                    // still recover the name instead of silently dropping
                    // the tag. The remembered fallback is plain text — see
                    // the `None` arm below for why that is an acceptable,
                    // disclosed narrowing rather than a silent one.
                    crate::remote_skins::remember_name(id, &plain);
                    Some(styled)
                }
            }
            // No tab-list entry for this uuid *this frame* -- not the same
            // thing as "this player has no name". Prefer whatever name was
            // last resolved for this uuid over drawing no tag at all.
            //
            // Only the plain string survives into `remember_name`'s cache
            // (a small, uuid-keyed fallback used by more than just this
            // call site), so a name recovered this way draws unstyled. This
            // is narrower than the metadata-flattening bug this fix closes:
            // it only degrades a tag on the specific frame a tab-list entry
            // is transiently missing, not on every frame for every entity.
            None => crate::remote_skins::last_known_name(&id).map(Text::literal),
        })
    } else {
        match &custom_name {
            Reported::Reported(Some(text))
                if custom_name_visible == Some(true)
                    && !is_blank_name_tag(&text.to_plain_string()) =>
            {
                Some(text.clone())
            }
            _ => None,
        }
    };
    let name_tag = name_tag.map(|text| NameTag {
        // Carried through as a real `Text` — `gpu/nametag.rs`'s
        // `push_entity_quads` reads it with `Text::to_spans` directly, so
        // colour/bold/italic/underline/strikethrough (hex included) survive
        // all the way to the drawn vertex, with no legacy-string round trip
        // to lose a hex colour along the way.
        text,
        // `Entity.isDiscrete()`'s shift-key bit (`0x02`) — unknown (no
        // metadata yet) defaults open, matching every other not-yet-reported
        // boolean here.
        see_through: flags.map_or(true, |f| f & 0x02 == 0),
    });

    Some(EntityFacts {
        id,
        type_path: type_key.path().to_string(),
        feet: to_glam_vec3(position),
        yaw: body_yaw,
        head_yaw,
        pitch: rotation.pitch,
        scale,
        item,
        velocity: entity.get::<Velocity>().map(|v| to_glam_vec3(v.0)),
        on_ground: entity.get::<OnGround>().is_some_and(|grounded| grounded.0),
        equipment,
        equipment_dye,
        equipment_trim,
        variant: entity.get::<Variant>().map(|variant| variant.0.clone()),
        count,
        foil,
        item_dyed_color,
        item_potion_color,
        name_tag,
        creeper_swell_dir: entity.get::<CreeperSwellDir>().map(|dir| dir.0),
        // The same `tab_list` the nametag above came out of — a player's skin
        // and their display name are two fields of one profile, so there is no
        // second lookup and no second source of truth. Gated on `is_player`
        // rather than on the property being absent, so a server that attached a
        // `textures` property to a non-player profile cannot put a skin on a
        // mob's rig.
        player_skin: is_player.then(|| uuid).flatten().map(|id| {
            player_skin_for_uuid(id, tab_list)
        }),
    })
}

/// The skin one player uuid resolves to against `tab_list`, with the whole
/// fallback ladder vanilla's `SkinManager` has: the declared `textures`
/// property, then this session's last resolution for that uuid, then the
/// uuid-hash built-in identity.
///
/// # Why this is a named symbol rather than an inline closure
///
/// It has **two** production callers, and they are the two halves of one
/// question. [`resolve_entity_facts`] asks it for every *other* player in view;
/// `Sim::local_player_skin` asks it for **us**, because the local player has no
/// tracked entity — `extract_entity_draws` deliberately excludes it — and so
/// reaches none of this fold. Our own body and arm are drawn by a separate
/// producer entirely, and for as long as that producer had no skin resolution
/// of its own, the visible result was exactly the report this closes: every
/// other player wearing their own skin while our own first-person arm and
/// third-person body wore the pack default.
///
/// Sharing the ladder rather than reimplementing it is the point. A second copy
/// would be free to drift on any of the three rungs, and two of them
/// (`last_known`, and the identity pick) exist precisely because a naive
/// re-derivation looked right and was not.
#[must_use]
pub fn player_skin_for_uuid(
    id: uuid::Uuid,
    tab_list: &lodestone_game::tablist::TabList,
) -> crate::remote_skins::RemoteSkin {
    let Some(entry) = tab_list.get(&id) else {
        // No tab-list entry for this uuid *this frame*. This is not
        // the same thing as "this player has no skin": a
        // `player_info_remove` clears the entry outright, and a
        // player-type NPC whose plugin adds a tab-list entry (with
        // `textures`) and then removes it shortly after — keeping a
        // fake player out of the visible player list while the
        // entity stays spawned — makes the lookup miss exactly the
        // way a real disconnect would. Falling back to the
        // uuid-hash default here (as this used to, unconditionally)
        // is what "skin enable[d] for a second... then changed back
        // to a default alex skin" was: the tab-list entry, and only
        // the tab-list entry, disappeared, so re-deriving from it
        // every frame silently discarded an already-resolved skin.
        // `remote_skins::last_known` is that resolution's memory —
        // the fetched texture is still sitting in `player_skins`'
        // GPU cache regardless, so prefer it over the default.
        return crate::remote_skins::last_known(&id)
            .unwrap_or_else(|| default_remote_skin(id));
    };
    match crate::remote_skins::skin_for_profile(&entry.profile) {
        Some(skin) => {
            crate::remote_skins::remember(id, &skin);
            skin
        }
        None => {
            // No declared `textures` property reached us: every
            // offline-mode server (whose profile carries no property at
            // all), and any online-mode account that has never set a skin.
            // Vanilla does **not** fall back to a fixed rig here either —
            // `SkinManager.registerTextures` still calls
            // `DefaultPlayerSkin.get(profileId)`, the uuid-hash pick over
            // the 18 built-in identities `lodestone_assets::skin`
            // (`default_skin_for_uuid`) now ports. Before this, every such
            // player was hardcoded wide (`type_path`, unmodified — see
            // `EntityDraw::model_type_path`), which is exactly the
            // "other/NPC players show a plain Steve" report: not a fetch
            // failure, a resolver that never ran for the common case.
            //
            // The empty `url` is a sentinel, not a real fetch target: the
            // draw's own fallback ("`Some(url)` with no bind group
            // installed yet resolves to the default sheet too" —
            // `EntityDrawBatch::skin`'s doc) already treats any unknown url
            // as "use the model's own sheet", and `remote_skins::request`
            // refuses an empty url outright so this can never open a
            // doomed HTTP GET. This branch (tab-list entry present, but
            // declaring no texture) is left at the plain default rather
            // than consulting `last_known`: unlike the missing-entry case
            // above, `fold_entry`'s merge rule keeps existing properties
            // whenever an update omits them, so an entry that is present
            // and genuinely declares no texture is trustworthy evidence
            // this player really has none, not a transient gap.
            default_remote_skin(id)
        }
    }
}

/// [`crate::remote_skins::RemoteSkin`]-shaped default for a player whose
/// profile declared no `textures` property — see the `or_else` above for why
/// this exists rather than leaving `player_skin` at `None`.
///
/// `url` is deliberately the empty string, never a real texture URL: nothing
/// downstream must ever attempt to fetch it, which is why
/// [`crate::remote_skins::request`] refuses an empty url before it can reach a
/// socket.
fn default_remote_skin(uuid: uuid::Uuid) -> crate::remote_skins::RemoteSkin {
    let (hi, lo) = uuid.as_u64_pair();
    let skin = lodestone_assets::skin::default_skin_for_uuid(hi as i64, lo as i64);
    crate::remote_skins::RemoteSkin {
        url: String::new(),
        model: skin.model,
        // The 18 hash-picked built-in identities carry no cape — vanilla's
        // `DefaultPlayerSkin` has none either.
        cape: None,
        // The whole point of the pick, and until this field existed it was
        // thrown away here: only `.model` (the rig) was read, so all eighteen
        // identities collapsed onto the pack's two plain sheets and every
        // skinless player was Steve or Alex.
        default_sheet: skin.texture,
    }
}

/// Fold this frame's entity state into the render component set: spawn tracks
/// for newly-seen entities, re-anchor eases for ones that moved or turned, and
/// prune everything [`EntityIndex`] no longer mentions.
///
/// # Replaces `fold_snapshots` + `net::entity_snapshots` (issue #36)
///
/// This used to take a `&[EntitySnapshot]` `sim.rs` built by calling
/// `NetClient::entity_snapshots()` — a *separate* read of the same `World`
/// this fold then took a write guard on, resolved to an owned `Vec` first only
/// to obey the no-reentrancy rule. Since §4.1(c) put ingest's components and
/// this module's render components in the one `World` `Sim` owns, that was a
/// redundant round trip: [`resolve_entity_facts`] below reads the ingest
/// components directly, inside this function's own write guard, at exactly
/// the position in the frame the fold already ran. That changes nothing about
/// *when* an ease begins — every numeric expectation the ~25 tests below pin
/// (clocks → ticks → fold, not the plan's `NetIngest` → `GameTick`) survives
/// unchanged, only each test's setup moves from building an `EntitySnapshot`
/// to spawning the ingest components directly. See
/// `docs/entity-components.md`'s "Update, and it changes the plan" for why
/// the schedule reorder issue #36's title implies is a separate change this
/// one does not need.
///
/// Skips any id [`EntityIndex`] maps to a [`LocalPlayer`] — the same filter
/// `lodestone_client::state::SharedState::entities` applies, written out
/// explicitly rather than left to fall out of the local player missing
/// [`EntityKind`]/[`Position`]/[`Rotation`]/[`HeadYaw`] (it would also be
/// excluded today by [`resolve_entity_facts`] returning `None` for it, which
/// is exactly the kind of accidental invariant that breaks silently the first
/// time someone adds one of those components to the local player for an
/// unrelated reason).
pub fn fold_entities(world: &mut World) {
    let tab_list = world
        .query_filtered::<&lodestone_ecs::SessionTabList, With<LocalPlayer>>()
        .iter(world)
        .next()
        .map(|list| list.0.clone())
        .unwrap_or_default();

    let tracked: Vec<(i32, Entity)> = world.resource::<EntityIndex>().iter().collect();
    let mut seen: HashSet<i32> = HashSet::with_capacity(tracked.len());

    for (id, ingest_entity) in tracked {
        let Ok(entity_ref) = world.get_entity(ingest_entity) else {
            continue;
        };
        if entity_ref.contains::<LocalPlayer>() {
            continue;
        }
        let Some(facts) = resolve_entity_facts(id, entity_ref, &tab_list) else {
            continue;
        };
        seen.insert(id);

        // **A recycled entity id must not inherit the previous tenant's stack.**
        // `ItemStacks` is keyed by server id alone, servers reuse ids freely,
        // and `Unreported` deliberately leaves an existing entry alone — so an
        // entity whose *kind* has changed since the last fold is the one case
        // where "leave it alone" is wrong. Dropping the entry here, before this
        // fold's own report is applied, is what stops a pig that inherits a
        // dropped stone's id from inheriting its stone.
        //
        // This used to be handled implicitly, by `extract_entity_draws`
        // refusing to read the table for anything but `ITEM_ENTITY_TYPE_PATH`.
        // That guard also refused it for item frames, framed maps and
        // projectiles — every other claimant of the same `ITEM_STACK`
        // serializer — so their contents never reached a pixel. The recycling
        // hazard is real and the type test was never the right instrument for
        // it: it answered "is this a drop?" when the question is "is this the
        // same entity the stack was reported for?".
        if let Some(entity) = world.resource::<TrackIndex>().0.get(&facts.id).copied()
            && world
                .get::<RenderKind>(entity)
                .is_some_and(|kind| kind.0.as_ref() != facts.type_path)
        {
            world.resource_mut::<ItemStacks>().0.remove(&facts.id);
        }

        // Fold the reported identity first, so a drop is never drawn for a frame
        // as a placeholder before its item lands. `Unreported` is "this fold
        // does not know", which must not clear what an earlier one established;
        // only an explicit empty stack clears.
        match &facts.item {
            Reported::Reported(Some(item)) => {
                world.resource_mut::<ItemStacks>().0.insert(
                    facts.id,
                    TrackedStack {
                        id: item.clone(),
                        count: facts.count,
                        foil: facts.foil,
                        dyed_color: facts.item_dyed_color,
                        potion_color: facts.item_potion_color,
                    },
                );
            }
            Reported::Reported(None) => {
                world.resource_mut::<ItemStacks>().0.remove(&facts.id);
            }
            Reported::Unreported => {}
        }

        match world.resource::<TrackIndex>().0.get(&facts.id).copied() {
            None => spawn_track(world, &facts),
            Some(entity) => update_track(world, entity, &facts),
        }
    }

    // Drop tracks for entities no longer reported — and the item stacks recorded
    // against them, or a long session leaks one entry per drop.
    let stale: Vec<(i32, Entity)> = world
        .resource::<TrackIndex>()
        .0
        .iter()
        .filter(|(id, _)| !seen.contains(id))
        .map(|(id, entity)| (*id, *entity))
        .collect();
    for (id, entity) in stale {
        world.despawn(entity);
        world.resource_mut::<TrackIndex>().0.remove(&id);
    }
    world
        .resource_mut::<ItemStacks>()
        .0
        .retain(|id, _| seen.contains(id));
}

/// The real-time ease window [`spawn_track`]/[`update_track`] should give this
/// server entity id's [`InterpClock`] this frame.
///
/// [`INTERP_WINDOW`] (three ticks) for everything: it exists to absorb the
/// jitter between one network-reported position and the next, which is real
/// for every entity we do not control. `id` gets [`TICK`] instead exactly
/// when it is the vehicle [`lodestone_ecs::vehicle::ControlledVehicle`] names —
/// see [`InterpClock::window`]'s own doc for why a locally-ticked, zero-jitter
/// source needs a narrower window rather than the network one, and
/// `riding_render_seat`'s doc for the on-screen symptom (the seat trailing the
/// vehicle's true motion under sustained acceleration) this fixes. `None`
/// resource (every harness that installs `EntityInterpPlugin` without
/// `LocalPlayerPlugin` — this module's own ~25 hermetic tests, and the live
/// GPU gates that drive `EntityInterpolator` directly) is "not riding
/// anything we control", the same as the resource being present at `None`.
fn interp_window_for(world: &World, id: i32) -> f32 {
    let is_our_vehicle = world
        .get_resource::<lodestone_ecs::vehicle::ControlledVehicle>()
        .and_then(|held| held.0.as_ref())
        .is_some_and(|held| held.server_id == id);
    if is_our_vehicle { TICK } else { INTERP_WINDOW }
}

/// A newly seen entity is drawn at rest at its reported pose: both ends of the
/// ease are the same, and the clock starts *finished* so nothing eases from
/// nowhere.
fn spawn_track(world: &mut World, snap: &EntityFacts) {
    let is_item = snap.type_path == ITEM_ENTITY_TYPE_PATH;
    let is_creeper = snap.type_path == "creeper";
    let window = interp_window_for(world, snap.id);
    let mut entity = world.spawn((
        MinecraftEntityId(snap.id),
        RenderKind(Arc::from(snap.type_path.as_str())),
        RenderScale(snap.scale),
        InterpFrom {
            feet: snap.feet,
            yaw: snap.yaw,
            head_yaw: snap.head_yaw,
            pitch: snap.pitch,
        },
        InterpTo {
            feet: snap.feet,
            yaw: snap.yaw,
            head_yaw: snap.head_yaw,
            pitch: snap.pitch,
        },
        InterpClock {
            t: window,
            age: 0.0,
            window,
        },
        WalkAnim {
            walk: WalkAnimation::new(),
            last_feet: snap.feet,
        },
        RenderEquipment(occupied_equipment(&snap.equipment)),
        RenderEquipmentDye(snap.equipment_dye.clone()),
        RenderEquipmentTrim(snap.equipment_trim.clone()),
        RenderWool(sheep_wool(&snap.type_path, snap.variant.as_ref())),
        RenderNameTag(snap.name_tag.clone()),
        RenderPlayerSkin(snap.player_skin.clone()),
        SwimRamp::IDLE,
        CapeLag::at(snap.feet),
    ));
    if is_item {
        entity.insert(new_item_physics(snap));
    }
    if is_creeper {
        entity.insert(CreeperFuse {
            swell_dir: snap.creeper_swell_dir.unwrap_or(CreeperFuse::IDLE.swell_dir),
            ..CreeperFuse::IDLE
        });
    }
    let entity = entity.id();
    world.resource_mut::<TrackIndex>().0.insert(snap.id, entity);
}

/// Fold a snapshot into an already-tracked entity.
///
/// A snapshot whose position or yaw differs from the current target starts a new
/// interpolation *from the current render pose*, so the mob never jumps. A
/// snapshot that matches the current target only lets the existing ease run to
/// completion.
fn update_track(world: &mut World, entity: Entity, snap: &EntityFacts) {
    // Computed against `world` before `get_entity_mut` below takes it — the
    // two borrows cannot overlap.
    let window = interp_window_for(world, snap.id);
    let Ok(mut entity) = world.get_entity_mut(entity) else {
        return;
    };
    let is_item = snap.type_path == ITEM_ENTITY_TYPE_PATH;

    if let Some(mut kind) = entity.get_mut::<RenderKind>() {
        // `Arc<str>` has no `clone_from`-style in-place reuse the way `String`
        // did, and a reported type essentially never changes update to
        // update — so skip the allocation (and the `Mut` write, avoiding
        // needless Bevy change-detection churn) entirely when it has not.
        if kind.0.as_ref() != snap.type_path.as_str() {
            kind.0 = Arc::from(snap.type_path.as_str());
        }
    }
    if let Some(mut scale) = entity.get_mut::<RenderScale>() {
        scale.0 = snap.scale;
    }
    // **Outside** the `moved || turned` gate below, deliberately. Equipment
    // changes with no movement at all — a mob picking up a dropped sword, a
    // plugin swapping a villager's hat, the player's own hotbar switch mirrored
    // back — and gating this on motion would leave a stationary mob holding
    // whatever it was holding when it last took a step.
    let occupied = occupied_equipment(&snap.equipment);
    if let Some(mut equipment) = entity.get_mut::<RenderEquipment>() {
        equipment.0 = occupied;
    }
    if let Some(mut dye) = entity.get_mut::<RenderEquipmentDye>() {
        dye.0.clone_from(&snap.equipment_dye);
    }
    // Same reasoning: a smithing table can trim a piece a player is already
    // wearing, which does not move them.
    if let Some(mut trim) = entity.get_mut::<RenderEquipmentTrim>() {
        trim.0.clone_from(&snap.equipment_trim);
    }
    // Same reasoning as equipment, outside the `moved || turned` gate: a sheep
    // can be sheared, or a plugin can dye one, while it stands still.
    let wool = sheep_wool(&snap.type_path, snap.variant.as_ref());
    if let Some(mut render_wool) = entity.get_mut::<RenderWool>() {
        render_wool.0 = wool;
    }
    // Same reasoning again: a player's tab-list name can change (a nickname
    // plugin, a rejoin under a different profile name) and a mob's
    // `CUSTOM_NAME_VISIBLE` can toggle, neither of which moves the entity.
    if let Some(mut name_tag) = entity.get_mut::<RenderNameTag>() {
        name_tag.0.clone_from(&snap.name_tag);
    }
    // Outside the motion gate for a stronger reason than the rest of this list:
    // `ADD_PLAYER` and `ADD_ENTITY` are separate packets, so a remote player's
    // first folds legitimately carry no profile and the skin has to be allowed
    // to arrive later — a player standing perfectly still while their tab-list
    // entry lands must still get their skin. See `RenderPlayerSkin`.
    if let Some(mut skin) = entity.get_mut::<RenderPlayerSkin>() {
        skin.0.clone_from(&snap.player_skin);
    }
    // Same "outside the motion gate" reasoning once more: a creeper's fuse
    // direction can flip while it stands still (backing away from a player it
    // was swelling toward). Only overwritten when *reported* — `None` means
    // this packet did not mention it, which must never reset a mid-fuse
    // creeper back to idle. See `EntityFacts::creeper_swell_dir`.
    if let Some(dir) = snap.creeper_swell_dir
        && let Some(mut fuse) = entity.get_mut::<CreeperFuse>()
    {
        fuse.swell_dir = dir;
    }

    let (Some(from), Some(to), Some(clock)) = (
        entity.get::<InterpFrom>().copied(),
        entity.get::<InterpTo>().copied(),
        entity.get::<InterpClock>().copied(),
    ) else {
        return;
    };
    let physics = entity.get::<ItemPhysics>().copied();

    // A dropped item's own simulation moves `InterpTo` every real tick (see
    // `tick_item_physics`), so comparing against it here would read as "moved"
    // every single frame even when the server has said nothing new since the
    // last poll. Compare against the last *authoritative* report instead —
    // `InterpTo` only for every other entity type, which the physics step never
    // touches.
    let moved = match &physics {
        Some(physics) => (snap.feet - physics.last_reported).length() > POS_EPS,
        None => (snap.feet - to.feet).length() > POS_EPS,
    };
    let turned = angle_diff(snap.yaw, to.yaw).abs() > YAW_EPS;
    let head_turned = angle_diff(snap.head_yaw, to.head_yaw).abs() > YAW_EPS;
    let pitched = (snap.pitch - to.pitch).abs() > YAW_EPS;
    if !(moved || turned || head_turned || pitched) {
        return;
    }

    // Re-anchor the ease at where the mob is drawn right now.
    let anchored = InterpFrom {
        feet: render_feet(&from, &to, &clock),
        yaw: render_yaw(&from, &to, &clock),
        head_yaw: render_head_yaw(&from, &to, &clock),
        pitch: render_pitch(&from, &to, &clock),
    };
    if let Some(mut current) = entity.get_mut::<InterpFrom>() {
        *current = anchored;
    }
    if let Some(mut target) = entity.get_mut::<InterpTo>() {
        target.feet = snap.feet;
        target.yaw = snap.yaw;
        target.head_yaw = snap.head_yaw;
        target.pitch = snap.pitch;
    }
    if let Some(mut clock) = entity.get_mut::<InterpClock>() {
        clock.t = 0.0;
        clock.window = window;
    }

    if !is_item {
        return;
    }
    match physics {
        Some(mut physics) => {
            physics.last_reported = snap.feet;
            physics.grounded = snap.on_ground;
            // Correct the simulation to the authoritative truth rather than
            // fight it — this is the "rare server correction" vanilla's own
            // local simulation also just snaps onto.
            physics.sim.position = to_model_vec3(snap.feet);
            if let Some(v) = snap.velocity {
                physics.sim.velocity = to_model_vec3(v);
            }
            physics.sim.on_ground = snap.on_ground;
            if let Some(mut current) = entity.get_mut::<ItemPhysics>() {
                *current = physics;
            }
        }
        None => {
            entity.insert(new_item_physics(snap));
        }
    }
}

/// Registers the render-side entity systems into the schedules `lodestone-ecs`
/// owns, plus the resources they read.
///
/// Still separate from `lodestone_ecs::ingest::IngestPlugin`, but no longer
/// because of a `World` boundary — §4.1(c) removed that, and the shell's one
/// `App` now installs both. It stays separate because the *entities* are still
/// separate: `IngestPlugin` folds the server's report onto one entity per mob and
/// this plugin's [`fold_entities`] spawns a second, render-side entity per mob
/// keyed by [`TrackIndex`], reading [`resolve_entity_facts`] as the bridge
/// between them (issue #36 deleted the `EntitySnapshot` type that used to
/// carry that bridge across a `Vec`).
///
/// Collapsing the two entities into one remains a separate, larger change —
/// ingest runs in `NetIngest`, which the plan orders *before* `GameTick`,
/// while this module's order is clocks → ticks → fold and every numeric
/// expectation in the ~25 tests below is written against it.
#[derive(Debug, Default)]
pub struct EntityInterpPlugin;

impl Plugin for EntityInterpPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FrameDelta>();
        app.init_resource::<ItemStacks>();
        app.init_resource::<TrackIndex>();
        app.init_resource::<ExtractedDraws>();
        // Issue #365. Written by `begin_item_pickup` from `Sim::poll_net`, aged by
        // `tick_pickup_animations`, drawn by `extract_pickup_draws`.
        app.init_resource::<PickupAnimations>();
        // `extract_entity_draws` reads `AttackSwing` through this — normally
        // `lodestone_ecs::ingest::IngestPlugin` owns it, but this plugin is
        // also installed alone by `EntityInterpolator::new()` (this module's
        // own harness and the live GPU gates), which never adds `IngestPlugin`
        // at all. `init_resource` is a no-op when it is already present, so
        // this does not race or double-initialize the production case where
        // both plugins are installed in the same `App`.
        app.init_resource::<EntityIndex>();
        // `Profile` is `lodestone_ecs::player`'s type, shared rather than
        // duplicated: the item integrator wants the same `PhysicsProfile` the
        // player's does, and since §4.1(c) they genuinely are one resource.
        // `ItemCollision`, by contrast, is deliberately *not* the player's
        // `PlayerCollision` — see its docs for the two decisions that differ.
        app.init_resource::<ItemCollision>();
        app.init_resource::<Profile>();
        app.add_systems(Update, advance_interp_clocks.in_set(FrameSet::Interpolate));
        app.add_systems(
            GameTick,
            tick_item_physics
                .in_set(TickSet::Physics)
                .before(tick_walk_animation),
        );
        app.add_systems(GameTick, tick_walk_animation.in_set(TickSet::Animate));
        app.add_systems(GameTick, tick_pickup_animations.in_set(TickSet::Animate));
        app.add_systems(GameTick, tick_creeper_fuse.in_set(TickSet::Animate));
        app.add_systems(GameTick, tick_swim_ramp.in_set(TickSet::Animate));
        app.add_systems(GameTick, tick_cape_lag.in_set(TickSet::Animate));
        // See `BodyYawState`'s doc: without this, a remote player's reported
        // body yaw and head yaw are the same wire number forever, and the
        // entity turns as one rigid block with no head lead.
        app.add_systems(GameTick, tick_remote_body_yaw.in_set(TickSet::Animate));
        app.add_systems(Extract, extract_entity_draws.in_set(ExtractSet::Entities));
        // **`.after` is load-bearing, not tidiness.** `extract_entity_draws`
        // clears `ExtractedDraws`; without the ordering, bevy is free to run this
        // first and have every appended pickup draw erased in the same frame it
        // was written — a system that runs, is unit-testable, and reaches zero
        // pixels.
        app.add_systems(
            Extract,
            extract_pickup_draws
                .in_set(ExtractSet::Entities)
                .after(extract_entity_draws),
        );
    }
}

/// Reset every render-side entity track, for a session teardown.
///
/// `Sim::end_session` used to do this by replacing the whole
/// [`EntityInterpolator`] — which also silently zeroed that `World`'s private
/// `TickAccum` while leaving the player's accumulator alone, re-phasing the two
/// clocks on every quit-to-title. There is one accumulator now and it is reset
/// explicitly (`FrameClock::reset_accumulator`), so the track teardown has to be
/// explicit too rather than a side effect of dropping a `World`.
pub fn reset_entity_tracks(world: &mut World) {
    let tracked: Vec<Entity> = world.resource::<TrackIndex>().0.values().copied().collect();
    for entity in tracked {
        if let Ok(entity) = world.get_entity_mut(entity) {
            entity.despawn();
        }
    }
    world.resource_mut::<TrackIndex>().0.clear();
    world.resource_mut::<ItemStacks>().0.clear();
    world.resource_mut::<ExtractedDraws>().0.clear();
    // A pickup in flight when the session ends has no collector to fly to any
    // more, and its start point is in a world we are leaving.
    world.resource_mut::<PickupAnimations>().0.clear();
}

/// What [`extract_entity_draws`] produced on the last `Extract` run.
#[must_use]
pub fn extracted_entity_draws(world: &World) -> Vec<EntityDraw> {
    world.resource::<ExtractedDraws>().0.clone()
}

/// Number of render-side entity tracks in `world`.
#[must_use]
pub fn tracked_entity_count(world: &World) -> usize {
    world.resource::<TrackIndex>().0.len()
}

/// Tracks and interpolates every visible entity between server ticks.
///
/// # This is a harness, not the production path, since §4.1(c)
///
/// It owns a `World` of its own. That used to be how the shell ran entity
/// interpolation, and it is why there were two `GameTick` schedules on two
/// accumulators. `lodestone_shell::sim::Sim` no longer holds one: it installs
/// [`EntityInterpPlugin`] in the one `App` and calls the free functions above.
///
/// What it is still for is a caller with **no driver** — the `#[ignore]`d live
/// GPU gates (`tests/live_entity_render.rs`, `tests/live_dropped_item.rs`) and
/// this module's own ~25 unit tests, which drive interpolation against a bare
/// `NetClient` with no `Sim` in sight. It runs the *same* systems in the same
/// order off the *same* [`lodestone_ecs::FrameClock`] type, so it is a second
/// instance of one mechanism rather than a second mechanism.
pub struct EntityInterpolator {
    world: World,
}

impl std::fmt::Debug for EntityInterpolator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never dump the whole `World`; the interesting scalar is how many
        // entities are tracked.
        f.debug_struct("EntityInterpolator")
            .field("tracked", &self.len())
            .finish_non_exhaustive()
    }
}

impl Default for EntityInterpolator {
    fn default() -> Self {
        Self::new()
    }
}

impl EntityInterpolator {
    /// A fresh interpolator with no tracked entities.
    ///
    /// Builds the `World` through an `App` because plugin `build` is the only
    /// way to register schedules and systems, then keeps the `World` and drops
    /// the `App` — azalea's own shape (`azalea-client/src/client.rs:143`), and
    /// the reason nothing here calls `App::update`.
    #[must_use]
    pub fn new() -> Self {
        let mut app = App::new();
        app.add_plugins((CorePlugin, EntityInterpPlugin));
        Self {
            world: std::mem::take(app.world_mut()),
        }
    }

    /// The `World` the render components live in, for a caller that wants to
    /// query or mutate them directly.
    ///
    /// This is the seam that keeps the component set from being an island: a
    /// plugin (or `Sim`, or a test) can read [`InterpTo`] and write
    /// [`InterpFrom`] on any tracked entity and the next
    /// [`extract_entity_draws`] run puts it on screen. It is also how §4.1's
    /// eventual World unification lands without changing this module: the driver
    /// will own the `App` and pass its `World` in rather than this type owning
    /// one.
    #[must_use]
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The mutable form of [`Self::world`].
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Record which item a dropped-item entity is carrying, so its
    /// [`EntityDraw`] can name a model to draw.
    ///
    /// # Where the live path calls this
    ///
    /// [`fold_entities`] does, from [`EntityFacts::item`], for every entity
    /// that carries a stack — the full live chain is `ITEM_STACK` metadata
    /// (index 8) → `EntityMetadataUpdate::item` →
    /// `lodestone_ecs::entity::DisplayItem` → [`resolve_entity_facts`] → here.
    /// It stays a public setter because it is also the direct seam for tests
    /// and for any caller that learns an item's identity outside the ingest
    /// component set.
    ///
    /// An item entity with no entry here draws nothing, which is also what
    /// vanilla does with an empty stack (`ItemEntityRenderer.submit` returns
    /// early on `state.item.isEmpty()`).
    ///
    /// Sets the count to `1` — the neutral value for a caller that only knows
    /// identity. See [`Self::set_item_stack_with_count`] to carry a real stack
    /// size through to [`EntityDraw::count`].
    pub fn set_item_stack(&mut self, entity_id: i32, item: ResourceLocation) {
        self.set_item_stack_with_count(entity_id, item, 1);
    }

    /// Same as [`Self::set_item_stack`], carrying a real stack size through to
    /// [`EntityDraw::count`].
    ///
    /// [`fold_entities`] calls this (via the same path as
    /// [`Self::set_item_stack`]'s doc comment describes) with
    /// [`EntityFacts::count`], which [`resolve_entity_facts`] reads straight
    /// off the wire's `ItemStack::count` — no model dependency needed to widen
    /// this far, per `docs/dropped-items.md`.
    pub fn set_item_stack_with_count(&mut self, entity_id: i32, item: ResourceLocation, count: u32) {
        self.world.resource_mut::<ItemStacks>().0.insert(
            entity_id,
            TrackedStack {
                id: item,
                count,
                foil: false,
                dyed_color: None,
                potion_color: None,
            },
        );
    }

    /// The item recorded for `entity_id`, if any.
    #[must_use]
    pub fn item_stack(&self, entity_id: i32) -> Option<&ResourceLocation> {
        self.world
            .resource::<ItemStacks>()
            .0
            .get(&entity_id)
            .map(|s| &s.id)
    }

    /// The stack count recorded for `entity_id`, if any item is recorded at
    /// all. `1` is the neutral default set by [`Self::set_item_stack`]; only
    /// [`Self::set_item_stack_with_count`] and the live [`fold_entities`]
    /// chain ever record anything else.
    #[must_use]
    pub fn item_count(&self, entity_id: i32) -> Option<u32> {
        self.world
            .resource::<ItemStacks>()
            .0
            .get(&entity_id)
            .map(|s| s.count)
    }

    /// [`Self::update_with_view`] against [`OpenAir`] (as a [`PlayerCollision::View`])
    /// and a default [`PhysicsProfile`] — i.e. the pre-collision behaviour,
    /// kept as the default entry point for tests and any caller with no world
    /// to query.
    ///
    /// **Not what the live path uses.** [`crate::sim::Sim`] calls
    /// [`Self::update_with_view`] with a real [`CollisionSource`], so a
    /// dropped item's fall actually stops at a floor — see that method's docs
    /// and the module docs on why an item needs its own physics at all.
    pub fn update(&mut self, dt: f32) {
        self.update_with_view(
            dt,
            PlayerCollision::View(Arc::new(OpenAir)),
            &PhysicsProfile::mc_1_21(),
        );
    }

    /// Advance every track by `dt` seconds, then fold this frame's ingest
    /// entity state.
    ///
    /// The order is load-bearing and is what the tests below are written
    /// against:
    ///
    /// 1. [`Update`] → [`advance_interp_clocks`]: every ease clock and age moves
    ///    on, so a fold that resets `t` this frame anchors from the pose
    ///    that was actually on screen.
    /// 2. per 20 Hz tick: [`GameTick`] → [`tick_item_physics`] (`TickSet::Physics`)
    ///    then [`tick_walk_animation`] (`TickSet::Animate`). Both run on a fixed
    ///    clock, not per frame.
    /// 3. [`fold_entities`]: this frame's ingest state, then the prune.
    /// 4. [`Extract`] → [`extract_entity_draws`], so [`Self::draws`] is a plain
    ///    read.
    ///
    /// Entities [`EntityIndex`] no longer maps are dropped (despawned/out of
    /// range) — a caller drives this by adding/removing ingest entities and
    /// their `EntityIndex` mapping on [`Self::world_mut`] before calling this,
    /// the same way [`crate::sim::Sim::fold_entities`] does against the live
    /// `World` (issue #36 deleted the `&[EntitySnapshot]` parameter this used
    /// to take; see [`fold_entities`]'s own doc for why).
    ///
    /// `collision`/`profile` feed only [`tick_item_physics`] (every other
    /// entity is a pure position ease and never touches either); they are
    /// inserted as resources before the tick loop so that system can be a real
    /// scheduled `Res` reader rather than a function this method calls by
    /// hand. `collision`'s view should cover wherever a tracked item entity
    /// actually is — `Sim::live_collision`'s 3×3-column-around-the-player
    /// snapshot is the intended source, so a drop far outside that radius
    /// still free-falls with no floor until it re-enters range, same as before
    /// this method existed.
    pub fn update_with_view(
        &mut self,
        dt: f32,
        collision: PlayerCollision,
        profile: &PhysicsProfile,
    ) {
        self.world.insert_resource(FrameDelta(dt));
        self.world.insert_resource(ItemCollision(collision));
        self.world.insert_resource(Profile(*profile));
        self.world.run_schedule(Update);

        // The one accumulator, in this harness's own `World`. Identical to what
        // `Sim::step` does with the driver's: `begin_frame` banks the clamped `dt`,
        // `take_tick` drains it, `end_frame` publishes the residual — which is the
        // partial tick `extract_entity_draws` reads, so it has to be published
        // before `Extract`.
        {
            let mut clock = self.world.resource_mut::<lodestone_ecs::FrameClock>();
            clock.begin_frame(f64::from(dt));
        }
        while self
            .world
            .resource_mut::<lodestone_ecs::FrameClock>()
            .take_tick()
        {
            // `tick_item_physics` runs as part of this schedule
            // (`TickSet::Physics`, ordered before `tick_walk_animation`'s
            // `TickSet::Animate`) — the server itself only *corrects* a
            // dropped item's position roughly once a second (`ItemEntity`'s
            // `updateInterval(20)`), so the arc has to come from here, not
            // from easing toward a sparse packet.
            self.world.run_schedule(GameTick);
        }
        self.world
            .resource_mut::<lodestone_ecs::FrameClock>()
            .end_frame();

        fold_entities(&mut self.world);

        self.world.run_schedule(Extract);
    }

    /// The interpolated draw list for this frame. Order is unspecified (grouped
    /// by model downstream), so no ordering guarantees are made here.
    ///
    /// A plain read of what [`extract_entity_draws`] produced at the end of the
    /// last [`Self::update_with_view`] — the extraction is not repeated here,
    /// because a `&self` method cannot run a schedule and because re-extracting
    /// per call would let two reads in one frame disagree.
    #[must_use]
    pub fn draws(&self) -> Vec<EntityDraw> {
        self.world.resource::<ExtractedDraws>().0.clone()
    }

    /// Number of entities currently tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.world.resource::<TrackIndex>().0.len()
    }

    /// Whether no entities are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.world.resource::<TrackIndex>().0.is_empty()
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
    use lodestone_ecs::entity::{
        CreeperSwellDir, CustomName, CustomNameVisible, DisplayItem, Equipment, EntityKind,
        EntityUuid, HeadYaw, OnGround, Position, Rotation, Variant, Velocity,
    };

    /// Test-only ingest builder replacing the deleted `EntitySnapshot` (issue
    /// #36): same field shape, but [`Self::apply`] spawns (or upserts) the
    /// real [`lodestone_ecs::entity`] components and registers the mapping in
    /// [`EntityIndex`], the same pattern
    /// `a_swinging_attack_swing_reaches_the_extracted_anim` already uses for a
    /// bare `AttackSwing`, generalised to every field the deleted type used to
    /// carry. This drives [`resolve_entity_facts`]'s real derivation rather
    /// than bypassing it, so these tests still exercise the code
    /// `fold_entities` runs live.
    ///
    /// A field left at `Reported::Unreported` / `None` / empty is simply never
    /// inserted, matching "the server has never mentioned this" — the same
    /// contract [`resolve_entity_facts`] reads back out. `.apply()` never
    /// *removes* a component, only inserts/overwrites, because no test here
    /// needs a reported field to revert to unreported.
    #[derive(Debug, Clone)]
    struct IngestSnap {
        id: i32,
        type_path: String,
        feet: Vec3,
        yaw: f32,
        head_yaw: f32,
        pitch: f32,
        item: Reported<ResourceLocation>,
        count: u32,
        velocity: Option<Vec3>,
        on_ground: bool,
        equipment: Vec<(EquipmentSlot, Option<ResourceLocation>)>,
        variant: Option<EntityVariant>,
        creeper_swell_dir: Option<i32>,
        experience_orb_value: Option<i32>,
    }

    impl IngestSnap {
        /// Spawns (first call for this id) or reuses (later calls, same
        /// pattern a real `SET_EQUIPMENT`/`move_entity` update would hit) the
        /// ingest entity `self.id` maps to, and inserts every component
        /// `self` reports.
        fn apply(&self, world: &mut World) {
            let entity = match world.resource::<EntityIndex>().get(self.id) {
                Some(existing) => existing,
                None => {
                    let entity = world.spawn(MinecraftEntityId(self.id)).id();
                    world.resource_mut::<EntityIndex>().insert(self.id, entity);
                    entity
                }
            };
            let mut e = world.entity_mut(entity);
            e.insert((
                EntityKind(self.type_path.parse().expect("valid type key")),
                Position(to_model_vec3(self.feet)),
                Rotation(lodestone_model::Rotation {
                    yaw: self.yaw,
                    pitch: self.pitch,
                }),
                HeadYaw(self.head_yaw),
                OnGround(self.on_ground),
                Equipment(
                    self.equipment
                        .iter()
                        .map(|(slot, item)| lodestone_model::EntityEquipment {
                            slot: *slot,
                            item: item
                                .as_ref()
                                .map(|loc| lodestone_model::ItemStack::new(resource_key(loc), 1)),
                        })
                        .collect(),
                ),
            ));
            match &self.item {
                Reported::Unreported => {}
                Reported::Reported(item) => {
                    e.insert(DisplayItem(
                        item.as_ref()
                            .map(|loc| lodestone_model::ItemStack::new(resource_key(loc), self.count)),
                    ));
                }
            }
            if let Some(v) = self.velocity {
                e.insert(Velocity(to_model_vec3(v)));
            }
            if let Some(variant) = &self.variant {
                e.insert(Variant(variant.clone()));
            }
            if let Some(dir) = self.creeper_swell_dir {
                e.insert(CreeperSwellDir(dir));
            }
            if let Some(value) = self.experience_orb_value {
                e.insert(ExperienceOrbValue(value));
            }
        }
    }

    /// A [`ResourceLocation`] and a [`lodestone_model::ResourceKey`] are the
    /// same namespace/path pair from two different crates — this is a field
    /// copy, the test-side mirror of [`resolve_entity_facts`]'s own conversion
    /// the other way.
    fn resource_key(loc: &ResourceLocation) -> lodestone_model::ResourceKey {
        lodestone_model::ResourceKey::new(loc.namespace(), loc.path())
            .expect("a valid ResourceLocation is always a valid ResourceKey")
    }

    /// Forgets `id`: removes it from [`EntityIndex`] and despawns the ingest
    /// entity it mapped to, so the next [`fold_entities`] treats it exactly
    /// like a server that has stopped reporting it — the test-side stand-in
    /// for "omitted from this frame's snapshots" now that there is no
    /// snapshot list to omit an entry from.
    fn forget(world: &mut World, id: i32) {
        if let Some(entity) = world.resource_mut::<EntityIndex>().remove(id) {
            world.despawn(entity);
        }
    }

    /// [`forget`] every currently-tracked id — the test-side stand-in for the
    /// old `interp.update(&[], dt)` ("nothing was reported this frame").
    fn forget_all(world: &mut World) {
        let ids: Vec<i32> = world.resource::<EntityIndex>().iter().map(|(id, _)| id).collect();
        for id in ids {
            forget(world, id);
        }
    }

    /// Builds a minimal ingest entity for [`resolve_entity_facts`] tests —
    /// only the components that function actually reads need real values,
    /// the rest are "never reported" by omission. The net.rs-era sibling of
    /// this was `bare_entity_view`, building an `EntityView` for the now-
    /// deleted `entity_snapshot` (issue #36): that boundary is now ingest
    /// components -> [`EntityFacts`], and [`resolve_entity_facts`] is called
    /// directly with an explicit id and an `EntityRef` rather than through
    /// [`EntityIndex`], the same way [`fold_entities`] calls it per tracked
    /// id — so these tests need no [`EntityIndex`]/[`MinecraftEntityId`] at
    /// all, unlike [`IngestSnap`].
    fn bare_entity(world: &mut World) -> Entity {
        world
            .spawn((
                EntityKind("minecraft:item".parse().expect("valid type key")),
                Position(to_model_vec3(Vec3::new(1.0, 64.0, 2.0))),
                Rotation(lodestone_model::Rotation { yaw: 0.0, pitch: 0.0 }),
                HeadYaw(0.0),
                OnGround(true),
            ))
            .id()
    }

    /// Resolves `entity` the same way [`fold_entities`] resolves a tracked
    /// id — id `9` throughout, matching the old `bare_entity_view`'s
    /// `entity_id: 9`, since nothing here asserts on [`EntityFacts::id`].
    fn facts_for(
        world: &World,
        entity: Entity,
        tab_list: &lodestone_game::tablist::TabList,
    ) -> EntityFacts {
        resolve_entity_facts(9, world.entity(entity), tab_list)
            .expect("bare_entity always carries the four required components")
    }

    /// The gap this fix closed: `SET_ENTITY_MOTION`/`add_entity` already
    /// decoded into a [`Velocity`] component, and [`OnGround`] has always
    /// been tracked — but the old `entity_snapshot` dropped both on the
    /// floor before they ever reached `EntitySnapshot`, so
    /// `EntityInterpolator` had no way to know a dropped item's velocity
    /// even though the wire data was sitting right there.
    #[test]
    fn resolve_entity_facts_carries_velocity_and_on_ground_through() {
        let mut world = World::new();
        let entity = bare_entity(&mut world);
        world.entity_mut(entity).insert((
            Velocity(to_model_vec3(Vec3::new(0.08, 0.2, 0.0))),
            OnGround(false),
        ));
        let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
        assert_eq!(
            facts.velocity,
            Some(Vec3::new(0.08, 0.2, 0.0)),
            "the decoded velocity must survive the ingest-components -> EntityFacts boundary"
        );
        assert!(!facts.on_ground);

        let mut world = World::new();
        let entity = bare_entity(&mut world);
        let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
        assert_eq!(
            facts.velocity, None,
            "a never-reported velocity must stay None, not collapse to zero"
        );
        assert!(facts.on_ground);
    }

    /// `ArmorStand.isSmall()` halves the whole model — issue #643's `small`
    /// clause, folded into [`EntityFacts::scale`] the same way [`Baby`]
    /// already is (a uniform half-scale approximating vanilla's separate
    /// small-model bake). Two arms, not one: `small: false` must leave scale
    /// at the ordinary `1.0` — without this half, a version that scaled
    /// *every* armour stand by `0.5` regardless of the flag would still pass
    /// a `small: true` -only assertion.
    #[test]
    fn resolve_entity_facts_halves_scale_for_a_small_armor_stand() {
        let mut world = World::new();
        let entity = bare_entity(&mut world);
        world.entity_mut(entity).insert(lodestone_ecs::entity::ArmorStandFlags {
            small: true,
            show_arms: false,
            no_base_plate: false,
            marker: false,
        });
        let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
        assert_eq!(facts.scale, 0.5, "small: true must halve the resolved scale");

        let mut world = World::new();
        let entity = bare_entity(&mut world);
        world.entity_mut(entity).insert(lodestone_ecs::entity::ArmorStandFlags {
            small: false,
            show_arms: false,
            no_base_plate: false,
            marker: false,
        });
        let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
        assert_eq!(
            facts.scale, 1.0,
            "small: false must leave scale at the ordinary 1.0, not halve it too"
        );

        // Absence (never reported) must also read as ordinary scale, per
        // this codebase's usual "unreported = the least surprising default"
        // rule for every other bool here.
        let mut world = World::new();
        let entity = bare_entity(&mut world);
        let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
        assert_eq!(
            facts.scale, 1.0,
            "an entity with no ArmorStandFlags at all must not be scaled down"
        );
    }

    /// The same shape of gap as the velocity fix above, one field over:
    /// `SET_EQUIPMENT` already folds into the [`Equipment`] component and the
    /// old `entity_snapshot` dropped it, so `EntityInterpolator` could never
    /// learn that a mob was holding anything.
    #[test]
    fn resolve_entity_facts_carries_equipment_through() {
        let mut world = World::new();
        let entity = bare_entity(&mut world);
        world.entity_mut(entity).insert(Equipment(vec![
            lodestone_model::EntityEquipment {
                slot: EquipmentSlot::MainHand,
                item: Some(lodestone_model::ItemStack::new(
                    "minecraft:diamond_sword".parse().expect("valid item key"),
                    1,
                )),
            },
            // An explicit clear: present in the list, empty in the slot. This
            // must survive as `Some(slot, None)`, not vanish.
            lodestone_model::EntityEquipment {
                slot: EquipmentSlot::Head,
                item: None,
            },
        ]));
        let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
        assert_eq!(
            facts.equipment.len(),
            2,
            "both an occupied and an explicitly-cleared slot must cross the boundary"
        );
        let main = facts
            .equipment
            .iter()
            .find(|(slot, _)| *slot == EquipmentSlot::MainHand)
            .expect("main hand survived");
        assert_eq!(
            main.1.as_ref().map(ToString::to_string).as_deref(),
            Some("minecraft:diamond_sword")
        );
        let head = facts
            .equipment
            .iter()
            .find(|(slot, _)| *slot == EquipmentSlot::Head)
            .expect("head slot survived");
        assert_eq!(
            head.1, None,
            "an explicitly-empty slot must stay present-and-empty, not be dropped"
        );

        // Control: a mob the server has said nothing about carries nothing, so
        // a consumer cannot mistake "no data" for "empty hands confirmed".
        let mut bare_world = World::new();
        let bare_entity_id = bare_entity(&mut bare_world);
        let bare = facts_for(
            &bare_world,
            bare_entity_id,
            &lodestone_game::tablist::TabList::new(),
        );
        assert!(bare.equipment.is_empty());
    }

    /// The last hop of `docs/armour-rendering.md`'s dye chain. The old
    /// `entity_snapshot` passed `Vec::new()` for the dye list unconditionally,
    /// so every leather item rendered undyed while the wire data sat inside
    /// the `Equipment` component's own `ItemStack`s.
    ///
    /// The expected value comes from outside our code: vanilla's own default
    /// leather RGB is the literal `10511680` in
    /// `ItemStackComponentizationFix.java`, which writes it as
    /// `dyed_color`'s `rgb` when an old stack carries no explicit colour.
    /// That is `0x00A06540`.
    #[test]
    fn resolve_entity_facts_carries_equipment_dye_through() {
        const VANILLA_DEFAULT_LEATHER: u32 = 0x00A0_6540;

        let dyed = |path: &str, colour: Option<u32>| {
            let mut stack =
                lodestone_model::ItemStack::new(path.parse().expect("valid item key"), 1);
            stack.components.dyed_color = colour;
            stack
        };

        let mut world = World::new();
        let entity = bare_entity(&mut world);
        world.entity_mut(entity).insert(Equipment(vec![
            lodestone_model::EntityEquipment {
                slot: EquipmentSlot::Chest,
                item: Some(dyed(
                    "minecraft:leather_chestplate",
                    Some(VANILLA_DEFAULT_LEATHER),
                )),
            },
            // An undyeable item in an occupied slot must contribute no entry
            // at all — not a zero, which would read as "dyed pure black".
            lodestone_model::EntityEquipment {
                slot: EquipmentSlot::Head,
                item: Some(dyed("minecraft:iron_helmet", None)),
            },
        ]));
        let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
        assert_eq!(
            facts.equipment_dye,
            vec![(EquipmentSlot::Chest, VANILLA_DEFAULT_LEATHER)],
            "only the dyed slot contributes, and it carries vanilla's exact RGB"
        );
        assert_eq!(
            facts.equipment.len(),
            2,
            "narrowing the dye list must not narrow `equipment` itself"
        );

        // Control: the same item with the dye component absent must produce
        // an empty list, so the assertion above cannot pass on a build that
        // simply forwards every occupied slot with some placeholder colour.
        let mut undyed_world = World::new();
        let undyed_entity = bare_entity(&mut undyed_world);
        undyed_world.entity_mut(undyed_entity).insert(Equipment(vec![
            lodestone_model::EntityEquipment {
                slot: EquipmentSlot::Chest,
                item: Some(dyed("minecraft:leather_chestplate", None)),
            },
        ]));
        let undyed = facts_for(
            &undyed_world,
            undyed_entity,
            &lodestone_game::tablist::TabList::new(),
        );
        assert!(
            undyed.equipment_dye.is_empty(),
            "no dye component reported means no dye, never a default"
        );
    }

    /// A third instance of the velocity/equipment gap: [`Variant`] was
    /// already fully decoded and the old `entity_snapshot` simply never read
    /// it. This is the fix `docs/entity-rendering.md`'s "Render layers: sheep
    /// wool" section describes as the missing last hop.
    #[test]
    fn resolve_entity_facts_carries_variant_through() {
        let mut world = World::new();
        let entity = bare_entity(&mut world);
        world.entity_mut(entity).insert(Variant(EntityVariant::Dyed {
            color: 14,
            sheared: false,
        }));
        let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
        assert_eq!(
            facts.variant,
            Some(EntityVariant::Dyed {
                color: 14,
                sheared: false
            }),
            "a decoded variant must survive the ingest-components -> EntityFacts boundary"
        );

        // Control: a mob the server has never sent a variant for must read as
        // `None`, not as some default variant.
        let mut bare_world = World::new();
        let bare_entity_id = bare_entity(&mut bare_world);
        let bare = facts_for(
            &bare_world,
            bare_entity_id,
            &lodestone_game::tablist::TabList::new(),
        );
        assert_eq!(bare.variant, None);
    }

    /// The last hop of the creeper-swell chain `docs/entity-rendering.md`'s
    /// "Creeper swell" section names: [`CreeperSwellDir`] is fully decoded —
    /// the old `entity_snapshot` was the one place that dropped it on the
    /// floor, hardcoding `None` regardless of what the server actually
    /// reported.
    #[test]
    fn resolve_entity_facts_carries_creeper_swell_dir_through() {
        let mut world = World::new();
        let entity = bare_entity(&mut world);
        world.entity_mut(entity).insert(CreeperSwellDir(1));
        let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
        assert_eq!(
            facts.creeper_swell_dir,
            Some(1),
            "a decoded swell direction must survive the ingest-components -> EntityFacts boundary"
        );

        // Control: an entity the server has never reported a swell direction
        // for (i.e. every non-creeper) must read as `None`, not some default
        // "growing" or "shrinking" direction.
        let mut bare_world = World::new();
        let bare_entity_id = bare_entity(&mut bare_world);
        let bare = facts_for(
            &bare_world,
            bare_entity_id,
            &lodestone_game::tablist::TabList::new(),
        );
        assert_eq!(bare.creeper_swell_dir, None);
    }

    /// The visible half of the stack-count gap `docs/dropped-items.md`
    /// describes: [`DisplayItem::count`](lodestone_model::ItemStack::count)
    /// was decoded all the way to the component and the old `entity_snapshot`
    /// dropped it exactly at this conversion, so a stack of 64 diamonds and a
    /// single diamond were indistinguishable past this point.
    #[test]
    fn resolve_entity_facts_carries_item_count_through() {
        let mut world = World::new();
        let entity = bare_entity(&mut world);
        world.entity_mut(entity).insert(DisplayItem(Some(
            lodestone_model::ItemStack::new(
                "minecraft:diamond".parse().expect("valid item key"),
                64,
            ),
        )));
        let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
        assert_eq!(facts.count, 64);

        // Control: no stack at all must read as the neutral `1`, not `0` — a
        // consumer that multiplies by count must never draw zero copies of
        // nothing.
        let mut bare_world = World::new();
        let bare_entity_id = bare_entity(&mut bare_world);
        let bare = facts_for(
            &bare_world,
            bare_entity_id,
            &lodestone_game::tablist::TabList::new(),
        );
        assert_eq!(bare.count, 1);
    }

    /// Issue #100's two nametag rules, each pinned directly against
    /// [`resolve_entity_facts`]'s real boundary rather than against the
    /// render path — the render-level pixel gate (`tests/nametag_pixels.rs`)
    /// proves the wiring end to end, this proves the *resolution logic* in
    /// isolation. Moved from `net.rs`'s now-deleted `entity_snapshot` tests
    /// (issue #36): the boundary these pin is ingest components ->
    /// `EntityFacts`, not `EntityView` -> `EntitySnapshot`.
    mod name_tag {
        use uuid::Uuid;

        use super::*;

        fn bare_player_entity(world: &mut World, uuid: Uuid) -> Entity {
            let entity = bare_entity(world);
            world.entity_mut(entity).insert((
                EntityKind("minecraft:player".parse().expect("valid type key")),
                EntityUuid(uuid),
            ));
            entity
        }

        /// The `textures` profile property reaches [`EntityFacts::player_skin`]
        /// and the slim rig reaches [`EntityDraw::model_type_path`] — the two
        /// halves that have to agree, checked through the same `tab_list`
        /// boundary the nametag above uses.
        ///
        /// Three things this pins, each of which fails differently:
        ///
        /// * a **mob** with the same property attached never gets a skin, so a
        ///   server cannot put a player sheet on a pig's rig;
        /// * a player whose profile declares **no** property is `None`, which is
        ///   every offline-mode server and must stay the ordinary path;
        /// * `model_type_path` returns `player_slim` for a slim declaration and
        ///   the untouched `type_path` otherwise — and the wide case is asserted
        ///   as `"player"`, **not** `"player_wide"`, because `type_path` is also
        ///   what `gpu/nametag.rs` feeds to `entity_dimensions`, where
        ///   `"player_wide"` is not a registry path and would fall back to a
        ///   default height.
        #[test]
        fn a_players_texture_property_reaches_the_draw_and_selects_its_rig() {
            fn textures_property(model: &str) -> lodestone_game::tablist::ProfileProperty {
                // Base64 of a minimal real payload. Built here rather than
                // borrowed from `remote_skins`' own fixture so this test does not
                // depend on that module's test-only helpers.
                let json = format!(
                    concat!(
                        r#"{{"textures":{{"SKIN":{{"url":"#,
                        r#""https://textures.minecraft.net/texture/deadbeef","#,
                        r#""metadata":{{"model":"{}"}}}}}}}}"#
                    ),
                    model
                );
                lodestone_game::tablist::ProfileProperty {
                    name: "textures".to_owned(),
                    value: base64(json.as_bytes()),
                    signature: None,
                }
            }
            fn base64(bytes: &[u8]) -> String {
                const T: &[u8; 64] =
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                let mut out = String::new();
                for chunk in bytes.chunks(3) {
                    let b = [
                        chunk[0],
                        chunk.get(1).copied().unwrap_or(0),
                        chunk.get(2).copied().unwrap_or(0),
                    ];
                    let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
                    for i in 0..4 {
                        if i <= chunk.len() {
                            out.push(char::from(T[((n >> (18 - 6 * i)) & 0x3f) as usize]));
                        } else {
                            out.push('=');
                        }
                    }
                }
                out
            }

            for (declared, expect_slim) in [("slim", true), ("default", false)] {
                let id = Uuid::from_u128(if expect_slim { 40 } else { 41 });
                let mut tabs = lodestone_game::tablist::TabList::new();
                let mut profile = lodestone_game::tablist::GameProfile::new(id, "Skinned");
                profile.properties.push(textures_property(declared));
                tabs.insert(lodestone_game::tablist::PlayerListEntry::new(profile));

                let mut world = World::new();
                let entity = bare_player_entity(&mut world, id);
                let facts = facts_for(&world, entity, &tabs);
                let skin = facts
                    .player_skin
                    .clone()
                    .expect("a declared texture property must reach the facts");
                assert_eq!(skin.url, "https://textures.minecraft.net/texture/deadbeef");

                let draw = draw_with_skin(facts.player_skin.clone());
                if expect_slim {
                    assert_eq!(draw.model_type_path(), "player_slim");
                } else {
                    assert_eq!(
                        draw.model_type_path(),
                        "player",
                        "the wide rig must leave type_path alone -- `player_wide` is \
                         not an entity-type registry path"
                    );
                }
            }

            // A mob carrying the same property gets no skin: the gather is gated
            // on the entity actually being a player.
            let id = Uuid::from_u128(42);
            let mut tabs = lodestone_game::tablist::TabList::new();
            let mut profile = lodestone_game::tablist::GameProfile::new(id, "NotAPlayer");
            profile.properties.push(textures_property("slim"));
            tabs.insert(lodestone_game::tablist::PlayerListEntry::new(profile));
            let mut world = World::new();
            let mob = bare_entity(&mut world);
            world.entity_mut(mob).insert(EntityUuid(id));
            assert!(facts_for(&world, mob, &tabs).player_skin.is_none());

            // And a player whose profile declares nothing -- every offline-mode
            // server -- no longer collapses to a hardcoded wide/Steve default.
            // `SkinManager.registerTextures` falls through to
            // `DefaultPlayerSkin.get(profileId)` in exactly this case (no `SKIN`
            // texture entry), so `resolve_entity_facts` must too, keyed on the
            // same uuid the nametag above already reads off the same tab-list
            // entry. Uuid 43 is a discriminating input, not an arbitrary one: it
            // resolves to the *slim* rig, so a regression back to "always None,
            // always wide" — the exact pre-fix behaviour, and the bug behind the
            // "world renders Steve, inventory renders Alex" report — fails this,
            // where a uuid that happened to land on wide could not tell the two
            // apart.
            let plain = Uuid::from_u128(43);
            let mut tabs = lodestone_game::tablist::TabList::new();
            tabs.insert(lodestone_game::tablist::PlayerListEntry::new(
                lodestone_game::tablist::GameProfile::new(plain, "Offline"),
            ));
            let mut world = World::new();
            let entity = bare_player_entity(&mut world, plain);
            let facts = facts_for(&world, entity, &tabs);
            let skin = facts
                .player_skin
                .clone()
                .expect("a declared-nothing player must still resolve the uuid-hash default");
            assert_eq!(
                skin.url, "",
                "the default sentinel must never look like a real, fetchable URL"
            );
            let (hi, lo) = plain.as_u64_pair();
            let expected = lodestone_assets::skin::default_skin_for_uuid(hi as i64, lo as i64);
            assert_eq!(
                expected.model,
                lodestone_assets::PlayerModelType::Slim,
                "uuid 43 must be the discriminating input this test relies on"
            );
            assert_eq!(
                skin.model, expected.model,
                "resolve_entity_facts must thread this entity's own uuid into \
                 default_skin_for_uuid, not draw an unrelated default"
            );
            assert_eq!(draw_with_skin(facts.player_skin.clone()).model_type_path(), "player_slim");
        }

        /// A player-type entity's resolved skin must survive its tab-list
        /// entry disappearing.
        ///
        /// The owner-reported shape: a real skin resolves for a second, then
        /// reverts to the default Alex skin. A `player_info_remove` clears a
        /// uuid's tab-list entry outright, and a player-type NPC whose
        /// plugin adds a tab-list
        /// entry (carrying `textures`) and then removes it shortly after —
        /// keeping a fake player out of the visible player list while its
        /// entity stays spawned — makes `tab_list.get(&id)` miss exactly the
        /// way a real disconnect would. Before this fix, `resolve_entity_facts`
        /// re-derived `player_skin` from the tab list on every frame with no
        /// memory of a previous resolution, so that miss silently discarded
        /// an already-resolved real skin in favour of the uuid-hash default —
        /// even though the fetched texture was still sitting in
        /// `remote_skins`' own caches.
        ///
        /// Two frames against the *same* entity: first with the tab-list
        /// entry present (the real skin resolves and `remote_skins::remember`
        /// records it), then with an empty tab list for the same uuid (the
        /// entry is gone, mirroring `player_info_remove`) — the second
        /// resolve must still report the real skin. The trailing control
        /// uses a *different*, never-before-seen uuid against the same empty
        /// tab list, and must still fall back to the default: the fix must
        /// not have become "never show the default", only "don't forget a
        /// skin this uuid actually had".
        #[test]
        fn a_players_skin_survives_a_missing_tab_list_entry_once_resolved() {
            fn textures_property(url: &str) -> lodestone_game::tablist::ProfileProperty {
                let json = format!(
                    r#"{{"textures":{{"SKIN":{{"url":"{url}","metadata":{{"model":"slim"}}}}}}}}"#
                );
                lodestone_game::tablist::ProfileProperty {
                    name: "textures".to_owned(),
                    value: base64(json.as_bytes()),
                    signature: None,
                }
            }
            fn base64(bytes: &[u8]) -> String {
                const T: &[u8; 64] =
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                let mut out = String::new();
                for chunk in bytes.chunks(3) {
                    let b = [
                        chunk[0],
                        chunk.get(1).copied().unwrap_or(0),
                        chunk.get(2).copied().unwrap_or(0),
                    ];
                    let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
                    for i in 0..4 {
                        if i <= chunk.len() {
                            out.push(char::from(T[((n >> (18 - 6 * i)) & 0x3f) as usize]));
                        } else {
                            out.push('=');
                        }
                    }
                }
                out
            }

            let id = Uuid::from_u128(90_210);
            let url = "https://textures.minecraft.net/texture/issue678";
            let mut tabs = lodestone_game::tablist::TabList::new();
            let mut profile = lodestone_game::tablist::GameProfile::new(id, "Npc");
            profile.properties.push(textures_property(url));
            tabs.insert(lodestone_game::tablist::PlayerListEntry::new(profile));

            let mut world = World::new();
            let entity = bare_player_entity(&mut world, id);

            let listed = facts_for(&world, entity, &tabs);
            let skin = listed
                .player_skin
                .clone()
                .expect("a declared texture property must resolve while listed");
            assert_eq!(skin.url, url);

            // The tab-list entry disappears (`player_info_remove`); the
            // entity itself is untouched.
            let unlisted = lodestone_game::tablist::TabList::new();
            let after_removal = facts_for(&world, entity, &unlisted);
            let skin_after = after_removal
                .player_skin
                .clone()
                .expect("a previously-resolved player must still report a skin");
            assert_eq!(
                skin_after.url, url,
                "a previously-resolved real skin must survive a missing tab-list \
                 entry rather than silently reverting to the uuid-hash default"
            );

            // Control: a uuid never seen through the tab list at all, against
            // the same empty tab list, must still take the default -- the
            // fallback is "remember what this uuid actually had", not "keep
            // showing the last skin resolved for anybody".
            let never_seen = Uuid::from_u128(90_211);
            let mut other_world = World::new();
            let other_entity = bare_player_entity(&mut other_world, never_seen);
            let never_facts = facts_for(&other_world, other_entity, &unlisted);
            let never_skin = never_facts
                .player_skin
                .clone()
                .expect("an unlisted player with no history still resolves the default");
            assert_eq!(
                never_skin.url, "",
                "a uuid with no prior real resolution must not spuriously inherit one"
            );
        }

        /// A minimal player [`EntityDraw`] carrying `skin` and nothing else —
        /// enough to exercise [`EntityDraw::model_type_path`], which is the only
        /// thing the caller asserts on.
        fn draw_with_skin(skin: Option<crate::remote_skins::RemoteSkin>) -> EntityDraw {
            EntityDraw {
                id: 1,
                type_path: std::sync::Arc::from("player"),
                variant_sheet: None,
                item: None,
                equipment: Vec::new(),
                equipment_dye: Vec::new(),
                equipment_trim: Vec::new(),
                wool: None,
                block_state: None,
            item_frame_rotation: 0,
                count: 1,
                foil: false,
                item_dyed_color: None,
                item_potion_color: None,
                feet: Vec3::ZERO,
                yaw: 0.0,
                head_yaw: 0.0,
                pitch: 0.0,
                scale: 1.0,
                anim: AnimInput::default(),
                name_tag: None,
                hurt: false,
                item_use: None,
                main_arm_left: false,
                creeper_swelling: 0.0,
                swim_amount: 0.0,
                death_time: 0.0,
                on_fire: false,
                invisible: false,
                armor_stand: None,
                player_skin: skin,
                // A player, not an experience orb.
                experience_orb_value: None,
                cape_sway: (0.0, 0.0, 0.0),
            }
        }

        /// A player's tag is always its tab-list display name —
        /// `Player.shouldShowName()` returns `true` unconditionally
        /// (`Player.java`), never gated on any metadata flag.
        #[test]
        fn a_player_entitys_tag_is_its_tab_list_display_name() {
            let id = Uuid::from_u128(1);
            let mut tabs = lodestone_game::tablist::TabList::new();
            tabs.insert(lodestone_game::tablist::PlayerListEntry::new(
                lodestone_game::tablist::GameProfile::new(id, "Steve"),
            ));

            let mut world = World::new();
            let entity = bare_player_entity(&mut world, id);
            let facts = facts_for(&world, entity, &tabs);
            assert_eq!(
                facts.name_tag.map(|t| t.text.to_plain_string()),
                Some("Steve".to_string()),
                "a player entity must show its tab-list name unconditionally"
            );
        }

        /// The other half: no matching tab-list entry (the player left, or a
        /// synthetic/demo entity claiming to be a player) draws nothing
        /// rather than a blank or placeholder tag.
        #[test]
        fn a_player_entity_with_no_tab_list_entry_has_no_tag() {
            let mut world = World::new();
            let entity = bare_player_entity(&mut world, Uuid::from_u128(2));
            let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
            assert_eq!(facts.name_tag, None);
        }

        /// A player-type entity's resolved name tag must survive its
        /// tab-list entry disappearing -- the same shape as
        /// `a_players_skin_survives_a_missing_tab_list_entry_once_resolved`,
        /// one field over. A plugin NPC (a fake player entity) commonly adds
        /// a tab-list entry, resolves its name and skin, then removes the
        /// entry while the entity stays spawned so it does not show up in
        /// the visible player list. The skin path already survives this
        /// (`remote_skins::last_known`); before this fix the name path did
        /// not, so the exact same server that keeps a player's skin visible
        /// could still make its nametag vanish.
        #[test]
        fn a_players_name_tag_survives_a_missing_tab_list_entry_once_resolved() {
            let id = Uuid::from_u128(555_444);
            let mut tabs = lodestone_game::tablist::TabList::new();
            tabs.insert(lodestone_game::tablist::PlayerListEntry::new(
                lodestone_game::tablist::GameProfile::new(id, "QuestGiver"),
            ));

            let mut world = World::new();
            let entity = bare_player_entity(&mut world, id);

            let listed = facts_for(&world, entity, &tabs);
            assert_eq!(
                listed.name_tag.map(|t| t.text.to_plain_string()),
                Some("QuestGiver".to_string()),
                "name must resolve while the entry is listed"
            );

            // The tab-list entry disappears (`player_info_remove`); the
            // entity itself is untouched.
            let unlisted = lodestone_game::tablist::TabList::new();
            let after_removal = facts_for(&world, entity, &unlisted);
            assert_eq!(
                after_removal.name_tag.map(|t| t.text.to_plain_string()),
                Some("QuestGiver".to_string()),
                "a previously-resolved name must survive a missing tab-list \
                 entry rather than silently dropping the tag"
            );

            // Control: a uuid never seen through the tab list at all, against
            // the same empty tab list, must still show no tag -- the fallback
            // is "remember what this uuid actually had", not "keep showing
            // the last name resolved for anybody".
            let never_seen = Uuid::from_u128(555_445);
            let mut other_world = World::new();
            let other_entity = bare_player_entity(&mut other_world, never_seen);
            let never_facts = facts_for(&other_world, other_entity, &unlisted);
            assert_eq!(
                never_facts.name_tag, None,
                "a uuid with no prior real resolution must not spuriously \
                 inherit another uuid's remembered name"
            );
        }

        /// Every other entity's tag is its `CUSTOM_NAME`, gated on
        /// `CUSTOM_NAME_VISIBLE` — `LivingEntity.shouldShowName() =
        /// isCustomNameVisible()` (`LivingEntity.java`/`:2365`), unlike
        /// a player.
        #[test]
        fn a_mob_with_a_visible_custom_name_shows_it() {
            let mut world = World::new();
            let entity = bare_entity(&mut world);
            world.entity_mut(entity).insert((
                CustomName(Some(Text::literal("Babe"))),
                CustomNameVisible(true),
            ));
            let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
            assert_eq!(facts.name_tag.map(|t| t.text.to_plain_string()), Some("Babe".to_string()));
        }

        /// The gate the base `Entity.shouldShowName()` predicate is: a
        /// custom name with `CUSTOM_NAME_VISIBLE` unset (or `false`) shows
        /// nothing, even though the name itself is known.
        #[test]
        fn a_mob_with_a_custom_name_but_not_visible_shows_nothing() {
            let mut world = World::new();
            let entity = bare_entity(&mut world);
            world.entity_mut(entity).insert((
                CustomName(Some(Text::literal("Babe"))),
                CustomNameVisible(false),
            ));
            let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
            assert_eq!(
                facts.name_tag, None,
                "CUSTOM_NAME_VISIBLE=false must suppress the tag even though a name is known"
            );

            // Same for "never reported" — the common case for most mobs.
            let mut bare_world = World::new();
            let bare_entity_id = bare_entity(&mut bare_world);
            let bare = facts_for(
                &bare_world,
                bare_entity_id,
                &lodestone_game::tablist::TabList::new(),
            );
            assert_eq!(bare.name_tag, None);
        }

        /// An explicitly empty custom name must not draw a zero-width
        /// visible tag — same rule the issue's scope names ("a non-empty
        /// custom name").
        #[test]
        fn a_mob_with_an_empty_custom_name_shows_nothing_even_if_visible() {
            let mut world = World::new();
            let entity = bare_entity(&mut world);
            world.entity_mut(entity).insert((
                CustomName(Some(Text::literal(""))),
                CustomNameVisible(true),
            ));
            let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
            assert_eq!(facts.name_tag, None);
        }

        /// **The owner's report**: an NPC with no name rendered the literal
        /// text `<empty>`. Nothing in this tree ever constructs that string
        /// (see [`is_blank_name_tag`]'s own doc) — a mob whose custom name
        /// arrives reading exactly `<empty>` must be treated the same as one
        /// whose name arrives as `""`, not drawn as if it were real content.
        #[test]
        fn a_mob_whose_custom_name_is_the_literal_empty_sentinel_shows_nothing() {
            let mut world = World::new();
            let entity = bare_entity(&mut world);
            world.entity_mut(entity).insert((
                CustomName(Some(Text::literal("<empty>"))),
                CustomNameVisible(true),
            ));
            let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
            assert_eq!(
                facts.name_tag, None,
                "a custom name of literally \"<empty>\" must draw no tag, not the \
                 literal text"
            );

            // Negative control: a real name is not caught by the same guard.
            let mut real_world = World::new();
            let real_entity = bare_entity(&mut real_world);
            real_world.entity_mut(real_entity).insert((
                CustomName(Some(Text::literal("<empty> the Cow"))),
                CustomNameVisible(true),
            ));
            let real_facts = facts_for(
                &real_world,
                real_entity,
                &lodestone_game::tablist::TabList::new(),
            );
            assert_eq!(
                real_facts.name_tag.map(|t| t.text.to_plain_string()),
                Some("<empty> the Cow".to_string()),
                "the guard must match the sentinel exactly, not merely contain it — \
                 a player who actually named their pet this must still see it"
            );
        }

        /// `Entity.isDiscrete()` (`isShiftKeyDown()`, bit 1 of the shared
        /// flags byte) gates the see-through pass off while sneaking
        /// (`SubmitNodeCollection.java`).
        #[test]
        fn sneaking_suppresses_see_through_but_not_the_tag_itself() {
            let mut world = World::new();
            let entity = bare_entity(&mut world);
            world.entity_mut(entity).insert((
                CustomName(Some(Text::literal("Babe"))),
                CustomNameVisible(true),
                EntityFlags(0x02), // FLAG_SHIFT_KEY_DOWN
            ));
            let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
            let tag = facts.name_tag.expect("the tag itself must still draw while sneaking");
            assert_eq!(tag.text.to_plain_string(), "Babe");
            assert!(!tag.see_through, "sneaking must suppress the see-through pass");
        }

        /// The default (no metadata reported yet) must not suppress
        /// see-through — most entities aren't sneaking.
        #[test]
        fn unknown_flags_default_to_see_through_enabled() {
            let mut world = World::new();
            let entity = bare_entity(&mut world);
            world.entity_mut(entity).insert((
                CustomName(Some(Text::literal("Babe"))),
                CustomNameVisible(true),
            ));
            assert!(
                world.entity(entity).get::<EntityFlags>().is_none(),
                "control: this test is about the unreported case"
            );
            let facts = facts_for(&world, entity, &lodestone_game::tablist::TabList::new());
            assert!(facts.name_tag.expect("tag must draw").see_through);
        }
    }

    /// A bow in the main hand, the shape [`arm_pose_for`] reads.
    fn bow_in_main_hand() -> Vec<(EquipmentSlot, ResourceLocation)> {
        vec![(
            EquipmentSlot::MainHand,
            "minecraft:bow".parse().expect("valid item key"),
        )]
    }

    /// Vanilla's `AbstractSkeletonRenderer.getArmPose` override, all four terms of
    /// its conjunction moved one at a time (issue #379).
    ///
    /// Each `false` case below is a way the bug could come back, and each is a
    /// *different* mechanism: the flag not arriving, the wrong renderer family, and
    /// the item not being where the rule looks.
    #[test]
    fn an_aggressive_skeleton_with_a_bow_draws_and_nothing_else_does() {
        // The positive.
        assert_eq!(
            arm_pose_for("skeleton", &bow_in_main_hand(), None, true, false).pose,
            ArmPose::BowAndArrow
        );
        // Every `AbstractSkeletonRenderer` subclass, so the type set is not just
        // "skeleton" with the others assumed.
        for kind in ["wither_skeleton", "stray", "bogged", "parched"] {
            assert_eq!(
                arm_pose_for(kind, &bow_in_main_hand(), None, true, false).pose,
                ArmPose::BowAndArrow,
                "{kind} is drawn by AbstractSkeletonRenderer and must get the draw too"
            );
        }
        // It lands in the main hand for a right-handed mob: vanilla's
        // `getMainArm() == arm` term.
        assert!(!arm_pose_for("skeleton", &bow_in_main_hand(), None, true, false).left_hand);
        // ...and in the *left* hand once `Mob.isLeftHanded()` is set — the term
        // this used to hardcode a right-handed answer for.
        assert!(arm_pose_for("skeleton", &bow_in_main_hand(), None, true, true).left_hand);

        // Not aggressive — the flag is the trigger, and this is the case every
        // skeleton in the world was stuck in before #379.
        assert_eq!(
            arm_pose_for("skeleton", &bow_in_main_hand(), None, false, false).pose,
            ArmPose::Empty
        );
        // Aggressive, bow, wrong renderer family. `AbstractZombieRenderer` and
        // `IllagerRenderer` have no such override, so a humanoid mob's arms hang.
        for kind in ["zombie", "husk", "drowned", "pillager"] {
            assert_eq!(
                arm_pose_for(kind, &bow_in_main_hand(), None, true, false).pose,
                ArmPose::Empty,
                "{kind} is not drawn by AbstractSkeletonRenderer and must not get the draw"
            );
        }
        // An avatar is the other kind of "wrong renderer family", and its expected
        // pose is NOT `Empty`: `AvatarRenderer` has no aggressive-bow override
        // either, so a bow-holding player falls through to the ordinary held-item
        // raise. Asserting `Empty` here would be asserting the mob answer for the
        // one renderer that does not give it.
        for kind in ["player", "mannequin"] {
            assert_eq!(
                arm_pose_for(kind, &bow_in_main_hand(), None, true, false).pose,
                ArmPose::Item,
                "{kind} is drawn by AvatarRenderer: no draw pose, but a raised arm"
            );
        }
        // Aggressive skeleton, bow in the *off* hand: vanilla reads
        // `getMainHandItem()`, so this is the rest pose.
        let off_hand = vec![(
            EquipmentSlot::OffHand,
            "minecraft:bow".parse::<ResourceLocation>().expect("key"),
        )];
        assert_eq!(
            arm_pose_for("skeleton", &off_hand, None, true, false).pose,
            ArmPose::Empty
        );
        // Aggressive skeleton, no bow at all.
        assert_eq!(
            arm_pose_for("skeleton", &[], None, true, false).pose,
            ArmPose::Empty
        );
        // And a *non-vanilla* `bow` is a different item, not a bow.
        let modded = vec![(
            EquipmentSlot::MainHand,
            "mypack:bow".parse::<ResourceLocation>().expect("key"),
        )];
        assert_eq!(
            arm_pose_for("skeleton", &modded, None, true, false).pose,
            ArmPose::Empty
        );
    }

    /// The aggressive override must not eat the using-item path #57 built: a
    /// *player* drawing a bow has no mob-flags byte at all, and still poses.
    #[test]
    fn the_using_item_path_still_works_for_a_non_aggressive_entity() {
        let drawing = ItemUse {
            using: true,
            off_hand: false,
            ticks: 5,
        };
        assert_eq!(
            arm_pose_for("player", &bow_in_main_hand(), Some(drawing), false, false).pose,
            ArmPose::BowAndArrow,
            "a remote player drawing a bow is #57's mechanism and must be untouched"
        );
        // ...including on a skeleton, where both mechanisms could apply. Vanilla's
        // `? :` puts the aggressive branch first, but the answer is the same pose,
        // so what matters is that neither path is shadowed into never firing.
        assert_eq!(
            arm_pose_for("skeleton", &bow_in_main_hand(), Some(drawing), false, false).pose,
            ArmPose::BowAndArrow
        );
    }

    /// `minecraft:diamond_sword`, an item with no pose of its own — so the only
    /// thing that can raise the arm is the held-item fallthrough.
    fn sword_in(slot: EquipmentSlot) -> Vec<(EquipmentSlot, ResourceLocation)> {
        vec![(
            slot,
            "minecraft:diamond_sword".parse().expect("valid item key"),
        )]
    }

    /// `AvatarRenderer.getArmPose`'s tail — `? SPEAR : ITEM` for **any** non-empty
    /// hand, in use or not — against `HumanoidMobRenderer`'s `? SPEAR : EMPTY`.
    ///
    /// # The discriminating input is the *renderer*, not the item
    ///
    /// Both hypotheses ("the fallthrough is universal" and "the fallthrough is
    /// avatar-only") give `Item` for a player, so a player-only gate measures that
    /// the code runs. The zombie and skeleton rows are where the two answers differ,
    /// and they are the reason this is not a one-line widening: a universal
    /// fallthrough poses every armed zombie, skeleton, husk and armour stand in a
    /// pose vanilla never shows.
    ///
    /// Mismatches are collected and asserted on the collection, so a regression
    /// reports every arm rather than aborting on the first.
    #[test]
    fn a_merely_held_item_raises_an_avatars_arm_and_no_mobs() {
        // (type_path, equipment, expected pose, expected left_hand)
        let cases: Vec<(&str, Vec<(EquipmentSlot, ResourceLocation)>, ArmPose, bool)> = vec![
            // Avatars: `AvatarRenderer.getArmPose` reaches `ITEM`.
            (
                "player",
                sword_in(EquipmentSlot::MainHand),
                ArmPose::Item,
                false,
            ),
            (
                "mannequin",
                sword_in(EquipmentSlot::MainHand),
                ArmPose::Item,
                false,
            ),
            // Off hand only: vanilla poses the arm belonging to that hand, so the
            // pose must move to the left arm rather than staying on the main one.
            // A version that always reported the main hand passes every other row.
            (
                "player",
                sword_in(EquipmentSlot::OffHand),
                ArmPose::Item,
                true,
            ),
            // Both hands full. Vanilla raises both arms; one pose is all
            // `ArmPoseChoice` can carry, and the main hand is the one that wins.
            (
                "player",
                vec![
                    (
                        EquipmentSlot::MainHand,
                        "minecraft:diamond_sword".parse().expect("key"),
                    ),
                    (
                        EquipmentSlot::OffHand,
                        "minecraft:torch".parse().expect("key"),
                    ),
                ],
                ArmPose::Item,
                false,
            ),
            // A modded item is still not empty, so vanilla's `isEmpty()` test passes
            // it through to `ITEM`. This is the opposite of the `bow` rule, where the
            // namespace is load-bearing because the pose is per-item.
            (
                "player",
                vec![(
                    EquipmentSlot::MainHand,
                    "mypack:widget".parse().expect("key"),
                )],
                ArmPose::Item,
                false,
            ),
            // Empty hands: nothing to hold, nothing to raise.
            ("player", Vec::new(), ArmPose::Empty, false),
            // `minecraft:air` in the slot is `ItemStack.isEmpty()`'s other half.
            (
                "player",
                vec![(
                    EquipmentSlot::MainHand,
                    "minecraft:air".parse().expect("key"),
                )],
                ArmPose::Empty,
                false,
            ),
            // Humanoid MOBS, the rows the two hypotheses disagree on. Every one of
            // these overrides `getArmPose` and delegates to `HumanoidMobRenderer`'s
            // `EMPTY` tail.
            (
                "zombie",
                sword_in(EquipmentSlot::MainHand),
                ArmPose::Empty,
                false,
            ),
            (
                "skeleton",
                sword_in(EquipmentSlot::MainHand),
                ArmPose::Empty,
                false,
            ),
            (
                "husk",
                sword_in(EquipmentSlot::MainHand),
                ArmPose::Empty,
                false,
            ),
            (
                "drowned",
                sword_in(EquipmentSlot::MainHand),
                ArmPose::Empty,
                false,
            ),
            (
                "piglin",
                sword_in(EquipmentSlot::MainHand),
                ArmPose::Empty,
                false,
            ),
            // An armour stand is a `LivingEntity` with equipment and a humanoid rig,
            // and `ArmorStandRenderer` sets no arm pose at all. It is the row that
            // makes the universal reading visibly wrong: every decorative stand
            // holding a sword would have lifted its arm.
            (
                "armor_stand",
                sword_in(EquipmentSlot::MainHand),
                ArmPose::Empty,
                false,
            ),
        ];

        let mut mismatches = Vec::new();
        for (kind, equipment, want_pose, want_left) in cases {
            let got = arm_pose_for(kind, &equipment, None, false, false);
            if got.pose != want_pose || got.left_hand != want_left {
                mismatches.push(format!(
                    "{kind} holding {equipment:?}: want {want_pose:?} left_hand={want_left}, \
                     got {:?} left_hand={}",
                    got.pose, got.left_hand
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} of the held-item arms are wrong:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    /// The in-use poses must still win over the held-item fallthrough, in both
    /// directions: a bow being drawn is `BowAndArrow` and not `Item`, and a crossbow
    /// being wound keeps its progress rather than collapsing to the flat raise.
    ///
    /// The seam is where the mistake would live — `in_use_arm_pose` returning
    /// `Some(Empty)` instead of `None` for its "we were never told" cases would have
    /// disabled the fallthrough silently, and returning `None` for a real in-use pose
    /// would have flattened every bow draw to a held-item raise.
    #[test]
    fn an_in_use_pose_outranks_the_held_item_fallthrough() {
        let drawing = ItemUse {
            using: true,
            off_hand: false,
            ticks: 5,
        };
        assert_eq!(
            arm_pose_for("player", &bow_in_main_hand(), Some(drawing), false, false).pose,
            ArmPose::BowAndArrow,
            "a drawn bow must not collapse into the flat held-item raise"
        );
        let winding = ItemUse {
            using: true,
            off_hand: false,
            ticks: 12,
        };
        let crossbow = vec![(
            EquipmentSlot::MainHand,
            "minecraft:crossbow"
                .parse::<ResourceLocation>()
                .expect("key"),
        )];
        assert_eq!(
            arm_pose_for("player", &crossbow, Some(winding), false, false).pose,
            ArmPose::CrossbowCharge {
                progress: 12.0 / CROSSBOW_CHARGE_TICKS
            }
        );
        // Using something we were never told about: the in-use half declines, and the
        // fallthrough then finds the hand genuinely empty. `Empty`, not a guess.
        assert_eq!(
            arm_pose_for("player", &[], Some(drawing), false, false).pose,
            ArmPose::Empty
        );
        // ...but a hand that IS occupied while a *different* hand claims to be in use
        // still gets the raise, because the item is held either way.
        assert_eq!(
            arm_pose_for(
                "player",
                &sword_in(EquipmentSlot::MainHand),
                Some(ItemUse {
                    using: true,
                    off_hand: true,
                    ticks: 3,
                }),
                false,
                false,
            )
            .pose,
            ArmPose::Item
        );
        // `using: false` is the resting case, and for an avatar resting-with-an-item
        // is now a raised arm rather than a hanging one.
        assert_eq!(
            arm_pose_for(
                "player",
                &sword_in(EquipmentSlot::MainHand),
                Some(ItemUse {
                    using: false,
                    off_hand: false,
                    ticks: 0,
                }),
                false,
                false,
            )
            .pose,
            ArmPose::Item
        );
    }

    /// The physical-arm XOR itself (issue: left-handed mobs render right-handed):
    /// [`ArmPoseChoice::left_hand`] must be `off_hand != main_arm_left`, not a bare
    /// copy of either operand — checked across all four combinations so a
    /// transposition of the two `bool`s cannot survive, per `CLAUDE.md`'s note that
    /// two adjacent same-typed fields (here, two `bool`s) coincide half the time by
    /// chance.
    #[test]
    fn left_handedness_xors_with_the_hand_the_item_is_in() {
        // Held-item fallthrough (`held_item_arm_pose`): main hand vs. off hand,
        // each crossed with both handedness values.
        let main = sword_in(EquipmentSlot::MainHand);
        let off = sword_in(EquipmentSlot::OffHand);
        assert!(
            !arm_pose_for("player", &main, None, false, false).left_hand,
            "right-handed, main-hand item -> right arm"
        );
        assert!(
            arm_pose_for("player", &main, None, false, true).left_hand,
            "left-handed, main-hand item -> left arm"
        );
        assert!(
            arm_pose_for("player", &off, None, false, false).left_hand,
            "right-handed, off-hand item -> left arm"
        );
        assert!(
            !arm_pose_for("player", &off, None, false, true).left_hand,
            "left-handed, off-hand item -> right arm"
        );

        // The in-use path (`in_use_arm_pose`) has the same XOR, independently —
        // drawing a bow in the main hand while left-handed must draw with the
        // left arm.
        let drawing_main = ItemUse {
            using: true,
            off_hand: false,
            ticks: 5,
        };
        let drawing_off = ItemUse {
            using: true,
            off_hand: true,
            ticks: 5,
        };
        assert!(
            !arm_pose_for("player", &bow_in_main_hand(), Some(drawing_main), false, false)
                .left_hand
        );
        assert!(
            arm_pose_for("player", &bow_in_main_hand(), Some(drawing_main), false, true)
                .left_hand
        );
        let off_hand_bow = vec![(
            EquipmentSlot::OffHand,
            "minecraft:bow".parse::<ResourceLocation>().expect("key"),
        )];
        assert!(
            arm_pose_for("player", &off_hand_bow, Some(drawing_off), false, false).left_hand
        );
        assert!(
            !arm_pose_for("player", &off_hand_bow, Some(drawing_off), false, true).left_hand
        );
    }

    fn snap(id: i32, feet: Vec3, yaw: f32) -> IngestSnap {
        IngestSnap {
            id,
            type_path: "pig".into(),
            feet,
            yaw,
            head_yaw: yaw,
            pitch: 0.0,
            item: Reported::Unreported,
            count: 1,
            velocity: None,
            on_ground: false,
            equipment: Vec::new(),
            variant: None,
            creeper_swell_dir: None,
            experience_orb_value: None,
        }
    }

    fn creeper_snap(id: i32, swell_dir: Option<i32>) -> IngestSnap {
        IngestSnap {
            type_path: "creeper".into(),
            creeper_swell_dir: swell_dir,
            ..snap(id, Vec3::ZERO, 0.0)
        }
    }

    fn orb_snap(id: i32, value: Option<i32>) -> IngestSnap {
        IngestSnap {
            type_path: EXPERIENCE_ORB_TYPE_PATH.into(),
            experience_orb_value: value,
            ..snap(id, Vec3::ZERO, 0.0)
        }
    }

    /// [`extract_entity_draws`] must carry an orb's `ExperienceOrbValue` through to
    /// [`EntityDraw::experience_orb_value`], because that field is the **only**
    /// switch `RenderState::prepare_orbs` has: `None` means "not an orb" and draws
    /// nothing at all.
    ///
    /// Three separate claims, and the middle one is the one a "does the field
    /// arrive?" test would miss:
    ///
    /// * an orb with a reported value carries **that** value, not a placeholder;
    /// * an orb with **no** reported value still carries `Some(0)` — vanilla's own
    ///   accessor default — so it draws sprite cell 0 rather than vanishing. A
    ///   `.map()` straight off the component would give `None` here and the orb
    ///   would be invisible for exactly as long as the server withheld the field;
    /// * a **pig** carries `None`, which is the negative control. Without it a
    ///   version that unconditionally wrote `Some(0)` would pass the first two and
    ///   turn every mob in the world into an orb sprite.
    #[test]
    fn an_orbs_value_reaches_the_draw_and_nothing_else_claims_to_be_an_orb() {
        let mut interp = EntityInterpolator::new();
        (orb_snap(1, Some(617))).apply(interp.world_mut());
        (orb_snap(2, None)).apply(interp.world_mut());
        (snap(3, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        let draws = interp.draws();
        let value_of = |id: i32| -> Option<i32> {
            draws
                .iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("no draw for entity {id}"))
                .experience_orb_value
        };
        // Collected rather than asserted in place, so one wrong arm does not hide
        // the other two.
        let mut wrong = Vec::new();
        if value_of(1) != Some(617) {
            wrong.push(format!("a reported orb value became {:?}", value_of(1)));
        }
        if value_of(2) != Some(0) {
            wrong.push(format!(
                "an unreported orb value became {:?}, not the vanilla default Some(0)",
                value_of(2)
            ));
        }
        if value_of(3).is_some() {
            wrong.push(format!("a pig reported an orb value of {:?}", value_of(3)));
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        // And the value has to be one that picks a *different* sprite cell from the
        // default, or this gate would pass against a version that discarded it.
        assert_ne!(
            lodestone_render::experience_orb_icon(617),
            lodestone_render::experience_orb_icon(0),
            "617 and 0 must bucket differently for the assertion above to mean anything"
        );
    }

    /// The render-side half of issue #10's fix: [`extract_entity_draws`] must
    /// read a swinging [`AttackSwing`] through [`EntityIndex`] and land it on
    /// [`EntityDraw::anim`]`.attack_anim`. `lodestone-ecs::ingest`'s own tests
    /// cover the producer (`SwingMainHand` → `AttackSwing`); this is the
    /// consumer, driven directly off the component rather than through a full
    /// `IngestPlugin` World, since this module's harness installs only
    /// `EntityInterpPlugin` (see [`EntityInterpolator::new`]'s doc).
    ///
    /// The **negative control** is the first assertion: the same track, before
    /// any `AttackSwing` exists at all, must read exactly `0.0` — the old
    /// hardcoded value `render_anim` used to return unconditionally. Without
    /// this control, a `swing_progress` that was wired to the wrong id (or
    /// never wired at all) could still pass on the strength of the second
    /// assertion alone, the same "control's premise can be false" trap
    /// `CLAUDE.md` warns about.
    #[test]
    fn a_swinging_attack_swing_reaches_the_extracted_anim() {
        let mut interp = EntityInterpolator::new();
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        assert_eq!(
            interp.draws()[0].anim.attack_anim,
            0.0,
            "no AttackSwing yet: rigid arm, the negative control"
        );

        // Exactly what `lodestone_ecs::ingest::apply_entity_animation` +
        // three `tick_entity_swing` runs would have produced for id 1 after a
        // `SwingMainHand` report: `swing_time` at 2 of a 6-tick swing.
        let mut swing = AttackSwing::default();
        swing.start_swing(6);
        swing.tick();
        swing.tick();
        swing.tick();
        let ingest_entity = interp.world_mut().spawn(swing).id();
        interp
            .world_mut()
            .resource_mut::<EntityIndex>()
            .insert(1, ingest_entity);

        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        let attack_anim = interp.draws()[0].anim.attack_anim;
        assert!(
            attack_anim > 0.1,
            "a mid-swing AttackSwing must reach the extracted anim, got {attack_anim}"
        );
    }

    /// Issue #98's render-side half, the same shape as the swing test above:
    /// [`extract_entity_draws`] must read [`HurtTime`] through [`EntityIndex`]
    /// and land it on [`EntityDraw::hurt`].
    ///
    /// Three states, not two, because `HurtTime` is **not removed** when it
    /// expires — `tick_hurt_time` saturates it at zero and leaves the component
    /// attached. A `hurt` wired as `hurts.get(entity).is_ok()` rather than
    /// `.0 > 0` would therefore leave every mob that was ever hit permanently
    /// red, and only the third assertion here can see that. The first is the
    /// negative control (`false` before any component exists, the value this
    /// field had hardcoded everywhere until now); without it, a `hurt` stuck at
    /// `true` would pass the second assertion on its own.
    #[test]
    fn a_ticking_hurt_time_reaches_the_extracted_draw_and_expires() {
        let mut interp = EntityInterpolator::new();
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        assert!(
            !interp.draws()[0].hurt,
            "no HurtTime yet: no overlay, the negative control"
        );

        // Exactly what `lodestone_ecs::ingest::apply_entity_damaged` inserts.
        let ingest_entity = interp.world_mut().spawn(HurtTime(10)).id();
        interp
            .world_mut()
            .resource_mut::<EntityIndex>()
            .insert(1, ingest_entity);
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        assert!(
            interp.draws()[0].hurt,
            "a live HurtTime must reach EntityDraw::hurt — this is issue #98's island"
        );

        // The expiry case: `tick_hurt_time` saturates at zero and leaves the
        // component in place, so a presence check would stay red forever.
        *interp
            .world_mut()
            .get_mut::<HurtTime>(ingest_entity)
            .expect("HurtTime was just inserted") = HurtTime(0);
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        assert!(
            !interp.draws()[0].hurt,
            "HurtTime(0) is an expired countdown, not an absent one — the overlay must clear"
        );
    }

    /// Issue #434's render-side half, the same shape as the hurt-time test
    /// above: [`extract_entity_draws`] must read [`EntityFlags`] through
    /// [`EntityIndex`] and land bit `0x01` on [`EntityDraw::on_fire`] —
    /// player report "mobs dont show flames yet".
    ///
    /// The negative control is asserted with `assert_eq!`, not merely
    /// `assert!(!…)`: `on_fire` must be **bit-identical** `false` when the
    /// byte is absent or has the bit clear, not just falsy by some looser
    /// comparison — an option-like boolean gated only in the `true` direction
    /// has already been a real defect in this codebase (`CLAUDE.md`'s
    /// evidence-standards section).
    #[test]
    fn an_entity_flags_bit_reaches_the_extracted_draw_as_on_fire() {
        let mut interp = EntityInterpolator::new();
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        assert_eq!(
            interp.draws()[0].on_fire,
            false,
            "no EntityFlags yet: no flame, the negative control"
        );

        // Exactly what `lodestone_ecs::ingest::apply_entity_metadata` inserts
        // from a shared-flags byte with only the crouch bit (0x02) set — the
        // control for "any EntityFlags at all" vs. "bit 0x01 specifically".
        let ingest_entity = interp.world_mut().spawn(EntityFlags(0x02)).id();
        interp
            .world_mut()
            .resource_mut::<EntityIndex>()
            .insert(1, ingest_entity);
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        assert_eq!(
            interp.draws()[0].on_fire,
            false,
            "EntityFlags present but bit 0x01 clear must still read false, \
             not merely truthy-flags"
        );

        // Now set bit 0x01 (on fire) alongside the still-set crouch bit —
        // proving this reads the specific bit, not "flags != 0".
        *interp
            .world_mut()
            .get_mut::<EntityFlags>(ingest_entity)
            .expect("EntityFlags was just inserted") = EntityFlags(0x03);
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        assert_eq!(
            interp.draws()[0].on_fire,
            true,
            "a live EntityFlags with bit 0x01 set must reach EntityDraw::on_fire \
             — this is issue #434's extraction half"
        );

        // Clearing bit 0x01 again (crouch bit left set) must clear on_fire —
        // proves this is read fresh each snapshot, not latched once true.
        *interp
            .world_mut()
            .get_mut::<EntityFlags>(ingest_entity)
            .expect("EntityFlags was just inserted") = EntityFlags(0x02);
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        assert_eq!(
            interp.draws()[0].on_fire,
            false,
            "clearing bit 0x01 must clear on_fire, not leave it latched"
        );
    }

    /// The full creeper-swell chain this pass wired, exercised end to end
    /// through the public `update` seam — `CreeperSwellDir`
    /// → `spawn_track`'s `CreeperFuse` insert → `tick_creeper_fuse` (driven by
    /// real 20 Hz ticks inside `update`, not hand-poked) →
    /// `EntityDraw::creeper_swelling`. Live player report: "the creeper ...
    /// doesnt expand/turn white or blink or whatever" — this is the render-side
    /// half of "expand".
    #[test]
    fn a_primed_creepers_swelling_rises_over_real_ticks_and_a_non_creeper_never_swells() {
        let mut interp = EntityInterpolator::new();

        // A non-creeper must read exactly 0.0, always — the default every
        // other entity type gets, at zero cost (no `CreeperFuse` component at
        // all; see `EntityDraw::creeper_swelling`'s doc).
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(1.0);
        assert_eq!(
            interp.draws()[0].creeper_swelling,
            0.0,
            "a non-creeper must never report a nonzero swell"
        );

        // A freshly spawned creeper whose first snapshot has not yet reported
        // a fuse direction: seeded from vanilla's own idle default (`-1`), so
        // it must read 0.0 too, the same as before this feature existed.
        (creeper_snap(2, None)).apply(interp.world_mut());
        interp.update(0.0);
        let draw_for = |interp: &EntityInterpolator, id: i32| -> EntityDraw {
            interp
                .draws()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("no draw for entity {id}"))
        };
        assert_eq!(
            draw_for(&interp, 2).creeper_swelling,
            0.0,
            "an unreported fuse direction must seed idle (-1), not swell"
        );

        // Fold `swell_dir = 1` first, at `dt = 0.0` (no ticks run yet) — `update`
        // runs this call's tick loop *before* folding this call's snapshot (see
        // its own doc's numbered order), so a direction change only takes effect
        // starting with the *next* call's ticks. Folding it alone first, then
        // ticking a full second in a follow-up call, is what actually exercises
        // `tick_creeper_fuse` at `swell_dir = 1`.
        (creeper_snap(2, Some(1))).apply(interp.world_mut());
        interp.update(0.0);
        (creeper_snap(2, Some(1))).apply(interp.world_mut());
        interp.update(1.0);
        let swelling = draw_for(&interp, 2).creeper_swelling;
        assert!(
            swelling > 0.0,
            "20 real ticks at swell_dir=1 produced no swelling at all — \
             tick_creeper_fuse is not reaching this entity (the island this test \
             exists to catch)"
        );
        // Vanilla's own divisor is `maxSwell - 2 = 28`; 20 ticks in one second
        // cannot have crossed 1.0 (that needs the fuse to reach 28+ ticks),
        // so this also catches a runaway integrator (e.g. ticking every frame
        // instead of every 20 Hz tick).
        assert!(
            swelling < 1.0,
            "20 ticks in produced swelling {swelling}, which should not yet be able to \
             exceed 1.0 (that needs ~28 ticks) — the tick rate looks wrong"
        );

        // And backing off (`swell_dir = -1`) must bring it back down, proving
        // the direction is actually read each update rather than latched once.
        // Same fold-then-tick shape as above.
        (creeper_snap(2, Some(-1))).apply(interp.world_mut());
        interp.update(0.0);
        (creeper_snap(2, Some(-1))).apply(interp.world_mut());
        interp.update(2.0);
        let receded = draw_for(&interp, 2).creeper_swelling;
        assert!(
            receded < swelling,
            "swelling did not recede after 2s at swell_dir=-1: was {swelling}, now {receded}"
        );
    }

    /// Issue #573's producer-side wiring, traced end to end: [`Pose`] on the
    /// ingest entity (what `apply_entity_metadata` inserts from the pose
    /// accessor at index 6) → [`SwimRamp`]/[`tick_swim_ramp`] →
    /// [`EntityDraw::swim_amount`]. The rotation math this value feeds is a
    /// separate, already-covered concern (`gpu::entity_passes`'s own
    /// `swim_rotation_interpolates_and_does_not_snap_at_a_threshold`); this
    /// test exists so a break in *this* link — the one `tick_swim_ramp` not
    /// running, or not being reached by `index`/`poses` — cannot hide behind
    /// that one passing.
    #[test]
    fn a_swimming_pose_ramps_swim_amount_and_it_reaches_the_draw() {
        let mut interp = EntityInterpolator::new();
        (snap(3, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        let draw_for = |interp: &EntityInterpolator, id: i32| -> EntityDraw {
            interp
                .draws()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("no draw for entity {id}"))
        };
        assert_eq!(
            draw_for(&interp, 3).swim_amount,
            0.0,
            "an entity that has never reported Pose::Swimming must read 0.0"
        );

        // `Pose` is not modelled by `IngestSnap` (crouching reads it the same
        // direct way — see `extract_entity_draws`'s own `poses` query), so it
        // is inserted straight onto the ingest entity here, exactly as
        // `apply_entity_metadata` would.
        let ingest_entity = interp
            .world_mut()
            .resource::<EntityIndex>()
            .get(3)
            .expect("entity 3 is spawned");
        interp
            .world_mut()
            .entity_mut(ingest_entity)
            .insert(Pose(lodestone_model::EntityPose::Swimming));

        // Two ticks (0.1s at 20 Hz) — short enough that `SWIM_AMOUNT_PER_TICK
        // = 0.09` cannot have saturated, so a nonzero-but-under-1.0 reading
        // is only possible if the ramp is actually integrating per tick.
        interp.update(0.1);
        let ramping = draw_for(&interp, 3).swim_amount;
        assert!(
            ramping > 0.0,
            "2 ticks with Pose::Swimming produced no ramp at all — \
             tick_swim_ramp is not reaching this entity (the island this \
             test exists to catch)"
        );
        assert!(
            ramping < 1.0,
            "2 ticks in produced swim_amount {ramping}, which should not yet \
             be able to reach 1.0 (that needs ~12 ticks) — the tick rate \
             looks wrong"
        );

        // `update` caps catch-up at `MAX_CATCH_UP_TICKS` (10) per call, so a
        // single `update(1.0)` does not advance a full 20 ticks — three calls
        // guarantee at least 30 more ticks regardless of that cap or of how
        // the first `update(0.1)` above rounded, which is enough to saturate
        // either way.
        for _ in 0..3 {
            interp.update(1.0);
        }
        assert_eq!(
            draw_for(&interp, 3).swim_amount,
            1.0,
            "well over enough ticks at Pose::Swimming must saturate at the vanilla clamp"
        );

        // Backing off — the pose reverts to standing — must bring the ramp
        // back down, proving `tick_swim_ramp` reads the pose fresh every
        // tick rather than latching the direction once.
        interp
            .world_mut()
            .entity_mut(ingest_entity)
            .insert(Pose(lodestone_model::EntityPose::Standing));
        interp.update(0.2);
        let receded = draw_for(&interp, 3).swim_amount;
        assert!(
            receded < 1.0,
            "swim_amount did not recede after leaving Pose::Swimming: still {receded}"
        );
    }

    /// The *second* consumer of the same ramp the test above proves reaches
    /// [`EntityDraw::swim_amount`]: [`render_anim`] now also threads it into
    /// [`EntityDraw::anim`]'s [`AnimInput::swim_amount`], which is the field
    /// `Skeleton::pose`'s humanoid swim branch (the arm-over-arm stroke, the
    /// leg kick, the head pitch) actually reads — `Skeleton::pose` never sees
    /// `EntityDraw::swim_amount` itself. Before this, `render_anim` did not
    /// take a `swim_amount` parameter at all, so `AnimInput` had no field to
    /// carry it and the arm-stroke animation could not exist no matter what
    /// `tick_swim_ramp` computed. A partial ramp (not `0.0`, not `1.0`) is the
    /// discriminating value: a `render_anim` that hardcoded `0.0` for the new
    /// field, or one that swapped it for an unrelated already-in-scope `f32`,
    /// would both pass a `0.0`-or-`1.0`-only check.
    #[test]
    fn the_swim_ramp_also_reaches_anim_input_not_just_the_top_level_field() {
        let mut interp = EntityInterpolator::new();
        (snap(3, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        let draw_for = |interp: &EntityInterpolator, id: i32| -> EntityDraw {
            interp
                .draws()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("no draw for entity {id}"))
        };
        let ingest_entity = interp
            .world_mut()
            .resource::<EntityIndex>()
            .get(3)
            .expect("entity 3 is spawned");
        interp
            .world_mut()
            .entity_mut(ingest_entity)
            .insert(Pose(lodestone_model::EntityPose::Swimming));

        // One tick — `SWIM_AMOUNT_PER_TICK == 0.09`, so this lands on a
        // partial ramp, not on the `0.0` both fields start at.
        interp.update(0.05);
        let draw = draw_for(&interp, 3);
        assert!(
            draw.swim_amount > 0.0,
            "precondition: the ramp must have moved off 0.0 for this test to discriminate"
        );
        assert_eq!(
            draw.anim.swim_amount, draw.swim_amount,
            "EntityDraw::anim.swim_amount ({}) must equal EntityDraw::swim_amount ({}) — \
             render_anim is not threading the ramp into AnimInput",
            draw.anim.swim_amount, draw.swim_amount
        );
    }

    /// The render-side half of the death animation: [`DeathTime`] on the ingest
    /// entity → [`EntityDraw::death_time`] **and** [`EntityDraw::hurt`], through the
    /// public `update` seam.
    ///
    /// Live player report: *"stuff dying doesnt have the death animation (the one
    /// where they turn red and tilt on their side)"*. Both halves are this one
    /// bridge: the tilt reads `death_time`, and the red is
    /// `hurtTime > 0 || deathTime > 0` — a disjunction whose second operand did not
    /// exist here before, so an untouched-since-death mob went red for ten ticks and
    /// then back to normal *before* it fell over.
    ///
    /// # `DeathTime(0)` at a non-zero partial tick is the discriminating input
    ///
    /// The frame is driven with `dt` of half a tick, so `partial_tick` is `0.5` and
    /// not zero — which is the only way to tell vanilla's
    /// `deathTime > 0 ? deathTime + partialTicks : 0.0F` from the bare
    /// `deathTime + partialTicks` anybody would write instead. At `DeathTime(0)`,
    /// the tick death is announced, the ternary gives `0.0` and the bare sum gives
    /// `0.5` — which would start the topple and the red mid-frame instead of on the
    /// tick boundary. **At any non-zero `DeathTime` the two agree**, so a gate that
    /// only ever set a live counter would measure that the bridge runs.
    ///
    /// # One frame per arm, because two half-ticks are a whole tick
    ///
    /// Each arm gets a **fresh** interpolator driven by exactly one half-tick frame,
    /// rather than one interpolator stepped repeatedly. Chaining two `update(0.025)`
    /// calls banks a full `TICK_PERIOD`, so `FrameClock::take_tick` claims it and
    /// `end_frame` publishes an `interp_alpha` of **0.0** — which silently turns the
    /// discriminating arm above back into a coincident one. Measured, not
    /// hypothesised: the first draft of this test chained the arms and the third arm
    /// reported `4.0` where 4.5 was predicted.
    ///
    /// Mismatches are collected rather than asserted arm by arm, so a neuter reports
    /// all four instead of aborting on the first.
    #[test]
    fn a_dying_entitys_death_time_reaches_the_draw_and_reddens_it() {
        // Half of one 20 Hz tick: `FrameClock` publishes `interp_alpha` 0.5 and
        // claims no tick, so nothing moves the counter underneath the assertion.
        const HALF_TICK_SECONDS: f32 = 0.025;

        // One frame of one entity, with `death` either absent (alive) or set to a
        // given tick count on the ingest entity — the same way this module's other
        // bridged components are exercised; the fold that really writes it lives in
        // `lodestone_ecs::ingest` and has its own gate there.
        let draw_with_death = |death: Option<u32>| -> EntityDraw {
            let mut interp = EntityInterpolator::new();
            (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
            if let Some(ticks) = death {
                let world = interp.world_mut();
                let entity = world
                    .resource::<EntityIndex>()
                    .get(1)
                    .expect("entity 1 is spawned");
                world.entity_mut(entity).insert(DeathTime(ticks));
            }
            interp.update(HALF_TICK_SECONDS);
            interp
                .draws()
                .into_iter()
                .find(|d| d.id == 1)
                .expect("no draw for entity 1")
        };

        let mut mismatches: Vec<String> = Vec::new();

        // Alive: no component at all, so both halves must read their resting value.
        let alive = draw_with_death(None);
        if alive.death_time != 0.0 {
            mismatches.push(format!(
                "a living entity reported death_time {} — absent DeathTime must read \
                 0.0, not a default",
                alive.death_time
            ));
        }
        if alive.hurt {
            mismatches.push("a living, unhurt entity must not carry the red overlay".into());
        }

        // The tick death is announced. The ternary must suppress the partial term.
        let announced = draw_with_death(Some(0));
        if announced.death_time != 0.0 {
            mismatches.push(format!(
                "DeathTime(0) at partial tick 0.5 reported death_time {} — vanilla's \
                 `deathTime > 0 ? deathTime + partialTicks : 0.0F` gives 0.0, and the \
                 bare sum gives 0.5, which starts the topple mid-frame",
                announced.death_time
            ));
        }
        if announced.hurt {
            mismatches.push(
                "DeathTime(0) must not redden the entity: vanilla's gate is \
                 `deathTime > 0`, and the killing blow's own HurtTime is what covers \
                 this one frame"
                    .into(),
            );
        }

        // Mid-fall. `4 + 0.5` exactly — a predicted value, not a direction. The 0.5
        // is also what proves the partial term is carried at all, which the arm
        // above can only prove is *suppressed*.
        let dying = draw_with_death(Some(4));
        if (dying.death_time - 4.5).abs() > 1e-6 {
            mismatches.push(format!(
                "DeathTime(4) at partial tick 0.5 reported death_time {}, want 4.5 — \
                 a bare 4.0 means the partial tick never reaches the draw, so the \
                 topple would step once per tick instead of easing",
                dying.death_time
            ));
        }
        if !dying.hurt {
            mismatches.push(
                "a dying entity must carry the red overlay off deathTime alone, with \
                 no HurtTime present — this is the disjunction's second operand, and \
                 the island this test exists to catch"
                    .into(),
            );
        }

        assert!(
            mismatches.is_empty(),
            "the DeathTime -> EntityDraw bridge is wrong:\n  {}",
            mismatches.join("\n  ")
        );
    }

    #[test]
    fn a_new_entity_is_drawn_at_its_reported_pose() {
        let mut interp = EntityInterpolator::new();
        (snap(1, Vec3::new(3.0, 64.0, -2.0), 90.0)).apply(interp.world_mut());
        interp.update(0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1);
        assert_eq!(draws[0].feet, Vec3::new(3.0, 64.0, -2.0));
        assert_eq!(draws[0].yaw, 90.0);
    }

    #[test]
    fn movement_interpolates_rather_than_snapping() {
        let mut interp = EntityInterpolator::new();
        // Establish the entity at the origin, its ease already complete.
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        // A new position arrives 4 blocks along +X.
        let target = Vec3::new(4.0, 0.0, 0.0);
        (snap(1, target, 0.0)).apply(interp.world_mut());
        interp.update(0.0);

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
        (snap(1, target, 0.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW / 2.0);
        let xm = interp.draws()[0].feet.x;
        assert!(
            xm > 0.5 && xm < 3.5,
            "half the window in, the mob should be mid-way, was x={xm}"
        );

        // A full window after the snapshot it reaches the target.
        (snap(1, target, 0.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
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
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        // One packet: the mob steps one block. No further packets arrive.
        (snap(1, Vec3::new(1.0, 0.0, 0.0), 0.0)).apply(interp.world_mut());
        interp.update(0.0);

        // Sample the drawn x each render frame for the next three ticks at 60 fps
        // and require it to keep increasing well past the first tick — a one-tick
        // window would have plateaued at x=1 by 50 ms.
        let frame = 1.0 / 60.0;
        let mut last = interp.draws()[0].feet.x;
        let mut advanced_after_one_tick = false;
        let mut elapsed = 0.0;
        while elapsed < INTERP_WINDOW - 1.0e-4 {
            (snap(1, Vec3::new(1.0, 0.0, 0.0), 0.0)).apply(interp.world_mut());
            interp.update(frame);
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
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        (snap(2, Vec3::X, 0.0)).apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(interp.len(), 2);
        // Entity 2 vanishes from the report. Since #36 deleted the
        // `&[EntitySnapshot]` parameter, re-applying only entity 1 no longer
        // *implies* 2 is gone — absence from a slice was the old signal, and
        // there is no slice. `forget` is the stand-in the migration added for
        // exactly this, and production's equivalent is `ingest.rs`'s
        // `ClientEvent::EntityRemoved` arm, which despawns and drops the
        // `EntityIndex` mapping the same way.
        forget(interp.world_mut(), 2);
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1, "the despawned entity must be gone");
    }

    #[test]
    fn yaw_interpolates_along_the_shortest_arc_across_the_wrap() {
        let mut interp = EntityInterpolator::new();
        (snap(1, Vec3::ZERO, 350.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        // Turn to 10°: the short way is +20° through 360/0, not −340° through 180.
        (snap(1, Vec3::ZERO, 10.0)).apply(interp.world_mut());
        interp.update(0.0);
        (snap(1, Vec3::ZERO, 10.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW / 2.0);
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
        s.apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        // Head turns to 10° (short arc +20° through 0), body stays at 0.
        s.head_yaw = 10.0;
        s.apply(interp.world_mut());
        interp.update(0.0);
        s.apply(interp.world_mut());
        interp.update(INTERP_WINDOW / 2.0);
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

    /// [`snap`] reports the same yaw for both `Rotation` and `HeadYaw`
    /// (`head_yaw: yaw` in its own literal), which is exactly a mob's
    /// spawn-time convention *and* a real player's wire convention every
    /// tick (`ServerEntity.sendChanges` sends `getYRot()` and
    /// `getYHeadRot()` — equal for a `Player`, since nothing ever moves the
    /// latter independently). Only the `type_path` differs from [`snap`]'s
    /// `"pig"`, which is what routes `tick_remote_body_yaw` at all — see
    /// [`BodyYawState`]'s own doc for why a `"pig"` must never take this
    /// path.
    fn player_snap(id: i32, feet: Vec3, yaw: f32) -> IngestSnap {
        IngestSnap {
            type_path: "player".into(),
            ..snap(id, feet, yaw)
        }
    }

    #[test]
    fn a_remote_players_body_lags_a_head_turn_instead_of_matching_it() {
        // The discriminating input this bug needs: a **player**, standing
        // still (`dx = dz = 0`, so `body_yaw_target`'s walking clause never
        // fires), whose reported yaw turns 30° from where it spawned. 30° is
        // comfortably *inside* vanilla's 50° `tickHeadTurn` drag threshold —
        // an angle at or past the clamp would drag the body under either
        // hypothesis and would prove nothing. Before `BodyYawState`/
        // `tick_remote_body_yaw` existed, `resolve_entity_facts` fed the one
        // wire number a player reports for both fields straight into
        // `EntityFacts::yaw`, so this exact setup reported `d.yaw == 40.0`
        // and `d.anim.head_yaw_deg == 0.0` — the "turns as one rigid block,
        // head never moves" report this fixes.
        let mut interp = EntityInterpolator::new();
        player_snap(1, Vec3::ZERO, 10.0).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);

        // Same re-anchor idiom as `yaw_interpolates_along_the_shortest_arc_
        // across_the_wrap` above: fold the changed snapshot at `dt = 0.0`
        // (no `GameTick` runs, so this captures only the render re-anchor),
        // then apply the identical value again and ease forward for real.
        player_snap(1, Vec3::ZERO, 40.0).apply(interp.world_mut());
        interp.update(0.0);
        player_snap(1, Vec3::ZERO, 40.0).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);

        let d = &interp.draws()[0];
        assert!(
            (d.yaw - 10.0).abs() < 1.0e-3,
            "a stationary player's body must not follow a head turn inside \
             the 50° clamp, was {}",
            d.yaw
        );
        assert!(
            (d.head_yaw - 40.0).abs() < 1.0e-3,
            "the head must still reach the newly reported yaw, was {}",
            d.head_yaw
        );
        assert!(
            (d.anim.head_yaw_deg - 30.0).abs() < 1.0e-3,
            "the relative head yaw the rig actually poses from must carry \
             the full 30° lead, was {}",
            d.anim.head_yaw_deg
        );
    }

    #[test]
    fn a_stationary_players_body_yaw_is_dragged_to_within_the_clamp() {
        // The other clause of `tick_head_turn` (ported once, at
        // `crate::sim::step::tick_head_turn`, and reused here rather than
        // re-implemented): standing still, a head turn *past* 50° must
        // instantly drag the body to exactly 50° behind the head, not merely
        // clamp the head's own rendered angle the way `clamp_head_to_body`'s
        // 75° Mob-only safety net does. Read `BodyYawState` directly rather
        // than through `EntityDraw`, so this test's prediction is not also
        // coupled to `InterpFrom`/`InterpTo`'s separate, already-tested
        // easing timing.
        let mut interp = EntityInterpolator::new();
        player_snap(1, Vec3::ZERO, 10.0).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);

        player_snap(1, Vec3::ZERO, 90.0).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);

        let entity = interp
            .world()
            .resource::<EntityIndex>()
            .get(1)
            .expect("the player's ingest entity must still be tracked");
        let state = interp
            .world()
            .get::<BodyYawState>(entity)
            .expect("a player must gain BodyYawState by its first GameTick");
        assert!(
            (state.yaw - 40.0).abs() < 1.0e-3,
            "an 80° head turn must drag the body to exactly 50° behind the \
             head (90° - 50° = 40°), was {}",
            state.yaw
        );
    }

    /// Drive a mob at a steady `v` blocks/tick for `ticks` server ticks, one
    /// packet per tick and one render frame per tick, and report the walk
    /// amplitude and the phase advanced over the last ten ticks.
    fn walk_at(v: f32, ticks: usize) -> (f32, f32) {
        let mut interp = EntityInterpolator::new();
        let mut pos = Vec3::ZERO;
        (snap(1, pos, 0.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        let mut phase_at_mark = 0.0;
        let mark = ticks.saturating_sub(10);
        for i in 0..ticks {
            pos.x += v;
            (snap(1, pos, 0.0)).apply(interp.world_mut());
            interp.update(TICK);
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
        (snap(1, pos, 0.0)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        for _ in 0..40 {
            pos.x += 0.1;
            (snap(1, pos, 0.0)).apply(interp.world_mut());
            interp.update(TICK);
        }
        assert!(interp.draws()[0].anim.limb_swing_amount > 0.2, "was walking");
        // The mob stops: same position reported for two seconds.
        for _ in 0..40 {
            (snap(1, pos, 0.0)).apply(interp.world_mut());
            interp.update(TICK);
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
        s.apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        s.pitch = 30.0;
        s.apply(interp.world_mut());
        interp.update(0.0);
        s.apply(interp.world_mut());
        interp.update(INTERP_WINDOW / 2.0);
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
        s.apply(interp.world_mut());
        interp.update(0.016);
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
        s.apply(interp.world_mut());
        interp.update(0.016);
        assert!(interp.draws()[0].equipment.is_empty());

        // Identical pose, new equipment.
        s.equipment = vec![(EquipmentSlot::MainHand, Some(sword()))];
        s.apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(
            interp.draws()[0].equipment,
            vec![(EquipmentSlot::MainHand, sword())],
            "equipment must not be gated on movement"
        );

        // ...and a wholesale replacement can take it away again, still without
        // moving. This is safe precisely because `EntityView::equipment` is the
        // accumulated set, never a delta.
        s.equipment = vec![(EquipmentSlot::MainHand, None)];
        s.apply(interp.world_mut());
        interp.update(0.016);
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
        s.apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(interp.draws()[0].equipment.len(), 1);
        forget_all(interp.world_mut());
        interp.update(0.016);
        assert!(interp.is_empty(), "the track itself must be pruned");
        assert!(interp.draws().is_empty());
    }

    // ---- sheep wool --------------------------------------------------------

    #[test]
    fn sheep_wool_narrows_only_the_dyed_variant_on_a_sheep() {
        let dyed = EntityVariant::Dyed {
            color: 5,
            sheared: false,
        };
        assert_eq!(
            sheep_wool("sheep", Some(&dyed)),
            Some(SheepWool {
                color: 5,
                sheared: false
            })
        );
        // The pig/cow trap `docs/entity-rendering.md` documents for the armour
        // attach applies here too: `AnimFamily::Quadruped` is shared by pig,
        // cow, sheep and wolf, so the gate must be the resolved type path,
        // never the family. A pig carrying the same variant (a plugin could
        // send this) must still grow no wool.
        assert_eq!(
            sheep_wool("pig", Some(&dyed)),
            None,
            "gating on family instead of type path would draw wool on a pig"
        );
        assert_eq!(
            sheep_wool("sheep", None),
            None,
            "no reported variant at all must not synthesise wool"
        );
        // A sheared sheep is still `Some` — the data stays honest about what
        // was reported; the draw-time skip belongs downstream, see
        // `SheepWool::sheared`'s doc comment.
        assert_eq!(
            sheep_wool(
                "sheep",
                Some(&EntityVariant::Dyed {
                    color: 0,
                    sheared: true
                })
            ),
            Some(SheepWool {
                color: 0,
                sheared: true
            })
        );
    }

    #[test]
    fn sheep_wool_reaches_the_draw_only_for_a_sheep() {
        let dyed = EntityVariant::Dyed {
            color: 10,
            sheared: false,
        };
        let mut sheep = snap(1, Vec3::ZERO, 0.0);
        sheep.type_path = "sheep".into();
        sheep.variant = Some(dyed.clone());
        let mut pig = snap(2, Vec3::new(1.0, 0.0, 0.0), 0.0);
        pig.variant = Some(dyed);

        let mut interp = EntityInterpolator::new();
        (sheep).apply(interp.world_mut());
        (pig).apply(interp.world_mut());
        interp.update(0.016);
        let draws = interp.draws();
        let sheep_draw = draws.iter().find(|d| d.id == 1).expect("sheep tracked");
        let pig_draw = draws.iter().find(|d| d.id == 2).expect("pig tracked");
        assert_eq!(
            sheep_draw.wool,
            Some(SheepWool {
                color: 10,
                sheared: false
            })
        );
        assert_eq!(
            pig_draw.wool, None,
            "the same decoded variant must not reach the draw on a non-sheep"
        );
    }

    #[test]
    fn shearing_updates_wool_on_a_sheep_that_has_not_moved() {
        // Mirrors `equipment_updates_on_a_mob_that_has_not_moved`: shearing is
        // a metadata update, not a movement, so it must not be gated on the
        // `moved || turned` check.
        let mut s = snap(1, Vec3::new(4.0, 64.0, 4.0), 90.0);
        s.type_path = "sheep".into();
        s.variant = Some(EntityVariant::Dyed {
            color: 3,
            sheared: false,
        });
        let mut interp = EntityInterpolator::new();
        s.apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(interp.draws()[0].wool.map(|w| w.sheared), Some(false));

        // Identical pose, freshly sheared.
        s.variant = Some(EntityVariant::Dyed {
            color: 3,
            sheared: true,
        });
        s.apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(
            interp.draws()[0].wool.map(|w| w.sheared),
            Some(true),
            "wool state must not be gated on movement, same as equipment"
        );
    }

    /// **The variant-sheet connectedness gate.** A wolf's decoded breed must reach
    /// [`EntityDraw::variant_sheet`] through the real `Extract` schedule.
    ///
    /// # Why this test exists at all
    ///
    /// `EntityTexture::resolve` was built, unit-tested and had **zero production
    /// readers** — the dual of this repo's usual island, and the species a
    /// connectedness scan structurally cannot see: `SET_ENTITY_DATA` decodes, the
    /// fold lands `Variant` on a component, and every consumer downstream asked for
    /// `default_path()`. So the question this asserts is not "does the packet
    /// arrive" but "does anything *read* it", and the only honest subject is the
    /// draw list the GPU pass consumes.
    ///
    /// # The discriminating input
    ///
    /// `minecraft:ashen`, not `minecraft:pale`. Pale is the default sheet, so a
    /// resolver that ignored the coat entirely would pass a pale arm — the
    /// coincident input. The pig arm is the negative half: the *same* mechanism must
    /// select a climate sheet, so the wiring is not wolf-shaped by accident; and the
    /// zombie arm proves a model with no variant axis stays `None` rather than
    /// picking up a neighbour's sheet.
    #[test]
    fn a_decoded_breed_reaches_the_draws_variant_sheet() {
        let keyed = |path: &str| {
            EntityVariant::Keyed(format!("minecraft:{path}").parse().expect("valid id"))
        };

        let mut wolf = snap(1, Vec3::ZERO, 0.0);
        wolf.type_path = "wolf".into();
        wolf.variant = Some(keyed("ashen"));
        // A second wolf of a *different* breed, so the two cannot both be satisfied
        // by one shared sheet — the same reason the breeds are required to be
        // distinct in `lodestone_render`'s own gate.
        let mut snowy = snap(2, Vec3::new(1.0, 0.0, 0.0), 0.0);
        snowy.type_path = "wolf".into();
        snowy.variant = Some(keyed("snowy"));
        // The climate axis, on the same wire shape.
        let mut pig = snap(3, Vec3::new(2.0, 0.0, 0.0), 0.0);
        pig.variant = Some(keyed("cold"));
        // No variant axis at all, carrying a variant anyway.
        let mut zombie = snap(4, Vec3::new(3.0, 0.0, 0.0), 0.0);
        zombie.type_path = "zombie".into();
        zombie.variant = Some(keyed("ashen"));
        // A wolf that has reported no variant: nothing to resolve, so the model's
        // own sheet applies and this must stay `None` rather than defaulting to pale
        // *through* the variant path (which would make the map lookup the authority
        // on a mob the server said nothing about).
        let mut unreported = snap(5, Vec3::new(4.0, 0.0, 0.0), 0.0);
        unreported.type_path = "wolf".into();

        let mut interp = EntityInterpolator::new();
        for s in [&wolf, &snowy, &pig, &zombie, &unreported] {
            s.apply(interp.world_mut());
        }
        interp.update(0.016);
        let draws = interp.draws();

        let mut wrong = Vec::new();
        for (id, want) in [
            (1, Some("entity/wolf/wolf_ashen")),
            (2, Some("entity/wolf/wolf_snowy")),
            (3, Some("entity/pig/pig_cold")),
            (4, None),
            (5, None),
        ] {
            match draws.iter().find(|d| d.id == id) {
                Some(draw) if draw.variant_sheet == want => {}
                Some(draw) => wrong.push(format!(
                    "id {id}: want {want:?}, got {:?}",
                    draw.variant_sheet
                )),
                None => wrong.push(format!("id {id}: not tracked at all")),
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    // ---- dropped items ---------------------------------------------------

    /// An item entity whose stack the server has not (yet) reported, and
    /// which has never reported a velocity — the pre-physics fallback path.
    fn item_snap(id: i32, feet: Vec3) -> IngestSnap {
        IngestSnap {
            id,
            type_path: ITEM_ENTITY_TYPE_PATH.into(),
            feet,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            item: Reported::Unreported,
            count: 1,
            velocity: None,
            on_ground: false,
            equipment: Vec::new(),
            variant: None,
            creeper_swell_dir: None,
            experience_orb_value: None,
        }
    }

    /// The same, carrying a reported stack, as the live path builds it.
    fn item_snap_with(id: i32, feet: Vec3, item: Option<ResourceLocation>) -> IngestSnap {
        IngestSnap {
            item: Reported::Reported(item),
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
    ) -> IngestSnap {
        IngestSnap {
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
        (item_snap(9, Vec3::new(1.0, 64.0, 2.0))).apply(interp.world_mut());
        interp.update(0.016);
        let draws = interp.draws();
        assert_eq!(draws.len(), 1, "an item entity must still be tracked");
        assert_eq!(draws[0].type_path.as_ref(), ITEM_ENTITY_TYPE_PATH);
        assert_eq!(draws[0].item, None);
        assert_eq!(draws[0].id, 9);
    }

    #[test]
    fn a_reported_stack_reaches_the_draw() {
        let mut interp = EntityInterpolator::new();
        interp.set_item_stack(9, stone());
        (item_snap(9, Vec3::new(1.0, 64.0, 2.0))).apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(interp.draws()[0].item, Some(stone()));
    }

    /// The stack count's own hop across the same boundary velocity/equipment
    /// crossed before it: `EntityFacts::count` -> `TrackedStack` ->
    /// `EntityDraw::count`, via `fold_entities` — the live path
    /// `net::entity_snapshot` feeds, not the setter.
    #[test]
    fn item_count_reaches_the_draw() {
        let mut interp = EntityInterpolator::new();

        // No stack reported at all: the neutral default, so a consumer that
        // multiplies by count never draws zero copies of nothing.
        (item_snap(9, Vec3::new(1.0, 64.0, 2.0))).apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(interp.draws()[0].count, 1);

        let mut with_count = item_snap_with(9, Vec3::new(1.0, 64.0, 2.0), Some(stone()));
        with_count.count = 64;
        with_count.apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(interp.draws()[0].item, Some(stone()));
        assert_eq!(interp.draws()[0].count, 64);
    }

    /// The direct setter seam, mirroring [`a_reported_stack_reaches_the_draw`]
    /// for the count half: [`EntityInterpolator::set_item_stack_with_count`]
    /// and its accessors.
    #[test]
    fn set_item_stack_with_count_is_recorded_and_reachable() {
        let mut interp = EntityInterpolator::new();
        interp.set_item_stack_with_count(9, stone(), 40);
        assert_eq!(interp.item_stack(9), Some(&stone()));
        assert_eq!(interp.item_count(9), Some(40));
        // The plain setter is documented as defaulting to the neutral count.
        interp.set_item_stack(9, stone());
        assert_eq!(interp.item_count(9), Some(1));
    }

    #[test]
    fn a_stack_is_only_attached_to_the_entity_it_was_reported_for() {
        // The failure this rules out: keying the lookup on anything but the
        // entity id (position, insertion order) makes every drop in a pile show
        // the first one's model.
        let mut interp = EntityInterpolator::new();
        interp.set_item_stack(9, stone());
        (item_snap(9, Vec3::ZERO)).apply(interp.world_mut());
        (item_snap(10, Vec3::X)).apply(interp.world_mut());
        interp.update(0.016);
        let draws = interp.draws();
        let with = draws.iter().filter(|d| d.item.is_some()).count();
        assert_eq!(with, 1, "only entity 9 was told what it is carrying");
        assert_eq!(
            draws.iter().find(|d| d.id == 9).unwrap().item,
            Some(stone())
        );
        assert_eq!(draws.iter().find(|d| d.id == 10).unwrap().item, None);
    }

    /// **A recycled entity id must not inherit the previous tenant's stack**,
    /// and that has to be true without asking whether the new tenant is a
    /// dropped item — which is the pair below.
    ///
    /// This gate used to be one assertion, `a_non_item_entity_never_carries_a
    /// _stack`, and it passed because `extract_entity_draws` refused to read
    /// the stack table for any type but `ITEM_ENTITY_TYPE_PATH`. That guard
    /// answered the wrong question: it also refused it for item frames, framed
    /// maps and thrown projectiles, all of which sync a stack through the very
    /// same `ITEM_STACK` serializer, so **their contents reached zero pixels**.
    /// The recycling hazard is real; the type test was never the instrument
    /// for it. `fold_entities` now drops the entry when a tracked id's *kind*
    /// changes, which is the actual invariant.
    #[test]
    fn a_recycled_id_drops_its_stack_but_a_reported_one_keeps_it() {
        // Arm 1, the hazard: entity 1 is a drop carrying stone, then the same
        // id comes back as a pig. Two folds, because one fold has no previous
        // tenant to inherit from and so cannot discriminate anything.
        let mut interp = EntityInterpolator::new();
        interp.set_item_stack(1, stone());
        (item_snap(1, Vec3::ZERO)).apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(
            interp.draws()[0].item,
            Some(stone()),
            "control: the drop must carry its stack, or arm 2 proves nothing",
        );
        (snap(1, Vec3::ZERO, 0.0)).apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(
            interp.draws()[0].item,
            None,
            "a pig inheriting a drop's id must not inherit its stone",
        );

        // Arm 2, the case the old guard broke: an entity that is *not* a drop
        // and reports a stack of its own keeps it. An item frame is the real
        // instance — `ItemFrame.DATA_ITEM` rides the same serializer.
        let mut interp = EntityInterpolator::new();
        let mut frame = snap(2, Vec3::ZERO, 0.0);
        frame.type_path = "item_frame".into();
        frame.item = Reported::Reported(Some(stone()));
        frame.apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(
            interp.draws()[0].item,
            Some(stone()),
            "a non-drop that reports its own stack must carry it to the draw",
        );
    }

    #[test]
    fn a_despawned_drop_takes_its_stack_with_it() {
        // Item entities are the highest-churn entity there is (every broken
        // block makes one, every one despawns after five minutes), so a stack
        // table that only grows is a real leak, not a theoretical one.
        let mut interp = EntityInterpolator::new();
        interp.set_item_stack(9, stone());
        (item_snap(9, Vec3::ZERO)).apply(interp.world_mut());
        interp.update(0.016);
        assert!(interp.item_stack(9).is_some());
        forget_all(interp.world_mut());
        interp.update(0.016);
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
        (item_snap_with(9, Vec3::ZERO, Some(stone()))).apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(interp.draws()[0].item, Some(stone()));
    }

    #[test]
    fn a_snapshot_silent_about_the_item_keeps_the_known_one() {
        // The regression this rules out is the whole reason `EntityFacts::item`
        // is nested: a drop reports its stack once at spawn and is silent
        // in every later metadata packet. Reading that silence as "empty" makes
        // the drop flicker into a placeholder one frame after it appeared.
        let mut interp = EntityInterpolator::new();
        (item_snap_with(9, Vec3::ZERO, Some(stone()))).apply(interp.world_mut());
        interp.update(0.016);
        (item_snap(9, Vec3::ZERO)).apply(interp.world_mut());
        interp.update(0.016);
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
        (item_snap_with(9, Vec3::ZERO, Some(stone()))).apply(interp.world_mut());
        interp.update(0.016);
        (item_snap_with(9, Vec3::ZERO, None)).apply(interp.world_mut());
        interp.update(0.016);
        assert_eq!(interp.draws()[0].item, None);
    }

    #[test]
    fn a_drop_interpolates_and_ages_like_any_other_entity() {
        // The bob and spin are driven by `anim.age_ticks`, so an item whose age
        // never advanced would hang motionless in the air.
        let mut interp = EntityInterpolator::new();
        (item_snap(9, Vec3::ZERO)).apply(interp.world_mut());
        interp.update(0.0);
        let first = interp.draws()[0].anim.age_ticks;
        (item_snap(9, Vec3::ZERO)).apply(interp.world_mut());
        interp.update(0.5);
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
        (item_snap_moving(9, spawn, Some(vel), false)).apply(interp.world_mut());
        interp.update(0.0);

        let mut max_y = interp.draws()[0].feet.y;
        // 40 ticks (2s) of real flight time with no further server packet —
        // matching the ~1/s correction cadence, this window has none at all.
        for _ in 0..40 {
            (item_snap_moving(9, spawn, Some(vel), false)).apply(interp.world_mut());
            interp.update(TICK);
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
        (item_snap_moving(9, spawn, None, false)).apply(interp.world_mut());
        interp.update(0.0);

        let mut max_y = interp.draws()[0].feet.y;
        for _ in 0..40 {
            (item_snap_moving(9, spawn, None, false)).apply(interp.world_mut());
            interp.update(TICK);
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
        (item_snap_moving(9, spawn, None, false)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);

        let mut max_y = interp.draws()[0].feet.y;
        for _ in 0..16 {
            (item_snap_moving(9, spawn, None, false)).apply(interp.world_mut());
            interp.update(TICK);
            max_y = max_y.max(interp.draws()[0].feet.y);
        }
        // The one late correction a real server would send once the item has
        // fallen under its own (server-side) gravity for about a second.
        let landed = Vec3::new(0.3, 63.2, 0.0);
        (item_snap_moving(9, landed, None, true)).apply(interp.world_mut());
        interp.update(TICK);
        for _ in 0..10 {
            (item_snap_moving(9, landed, None, true)).apply(interp.world_mut());
            interp.update(TICK);
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
        (item_snap_moving(9, resting, Some(Vec3::ZERO), true)).apply(interp.world_mut());
        interp.update(INTERP_WINDOW);
        for _ in 0..40 {
            (item_snap_moving(9, resting, Some(Vec3::ZERO), true)).apply(interp.world_mut());
            interp.update(TICK);
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
    #[derive(Debug)]
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

    impl CollisionSource for FlatFloor {
        fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
            f(self);
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
        let floor_y = 63;
        let floor: Arc<dyn CollisionSource> = Arc::new(FlatFloor { floor_y });
        let profile = PhysicsProfile::mc_1_21();
        let spawn = Vec3::new(0.5, 66.0, 0.5);
        // A real pop velocity (`ItemEntity`'s zero-arg constructor draws
        // `vy = 0.2`, small horizontal jitter), reported once and never again
        // — no further snapshot arrives for the rest of the test, matching
        // `updateInterval(20)`'s roughly-one-correction-per-second reality.
        let vel = Vec3::new(0.02, 0.2, 0.0);
        item_snap_moving(9, spawn, Some(vel), false).apply(interp.world_mut());
        interp.update_with_view(
            0.0,
            PlayerCollision::View(Arc::clone(&floor)),
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
            item_snap_moving(9, spawn, Some(vel), false).apply(interp.world_mut());
            interp.update_with_view(
                TICK,
                PlayerCollision::View(Arc::clone(&floor)),
                &profile,
            );
            min_y = min_y.min(interp.draws()[0].feet.y);
        }
        let final_feet = interp.draws()[0].feet;

        assert!(
            min_y >= floor_y as f32 + 1.0 - 1.0e-3,
            "the item's feet must never read below the floor's top surface \
             ({}), got a minimum of {min_y} — it sank through",
            floor_y + 1
        );
        assert!(
            (final_feet.y - (floor_y as f32 + 1.0)).abs() < 1.0e-2,
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
        (item_snap_moving(9, spawn, Some(vel), false)).apply(interp.world_mut());
        interp.update(0.0);
        for _ in 0..40 {
            (item_snap_moving(9, spawn, Some(vel), false)).apply(interp.world_mut());
            interp.update(TICK);
        }
        let final_y = interp.draws()[0].feet.y;
        assert!(
            final_y < 63.0,
            "the control must actually fall past the floor height (63) to \
             prove the positive test's floor is load-bearing; got {final_y}"
        );
    }
    // ---- the item-pickup fly-to-collector animation (issue #365) ----------

    /// The interpolant is **quadratic** in the age fraction, and the midpoint is
    /// where that matters: `ItemPickupParticleGroup` computes
    /// `time = (life + partial) / 3; time *= time`.
    ///
    /// A linear lerp — the obvious wrong reading, and the one the issue's own
    /// summary implies — puts the item at `0.5` of the way across when the truth is
    /// `0.25`. Half the flight is spent covering the first quarter of the distance,
    /// which is what makes the pickup read as a snap toward the player rather than a
    /// glide.
    #[test]
    fn the_pickup_ease_is_quadratic_not_linear() {
        assert!((pickup_progress(0.0, 0.0) - 0.0).abs() < 1e-6);
        assert!(
            (pickup_progress(1.5, 0.0) - 0.25).abs() < 1e-6,
            "halfway through the 3-tick flight the item must be a quarter of the way \
             there, not half; got {}",
            pickup_progress(1.5, 0.0)
        );
        assert!((pickup_progress(3.0, 0.0) - 1.0).abs() < 1e-6);
        // Clamped past the end rather than overshooting the collector.
        assert!((pickup_progress(4.0, 0.0) - 1.0).abs() < 1e-6);
    }

    /// **The end-to-end gate for #365, and the one that would have caught the
    /// island.** `begin_item_pickup` → `tick_pickup_animations` → the `Extract`
    /// schedule → an `EntityDraw` in the list `RenderState::prepare_item_geometry`
    /// consumes, at the position vanilla's own constants predict.
    ///
    /// The item is dropped from the second poll's snapshot list, exactly as the
    /// server drops it after `take_item_entity`: `fold_entities` despawns its track
    /// and prunes its `ItemStacks` entry, so a draw that still appears afterwards can
    /// only have come from the animation.
    ///
    /// Magnitude, not direction. One tick into the flight the progress is
    /// `(1/3)² = 1/9`, so with the collector 4 blocks away on `x` and its
    /// `y + 1.62/2 = 0.81` target height the item must be `4/9 ≈ 0.444` along `x`.
    /// A linear ease would put it at `4/3 ≈ 1.333` — three times further, and a
    /// "did it move?" assertion accepts both.
    ///
    /// The first poll's `dt` is **exactly `0.0`** so the frame clock banks no
    /// residual: `interp_alpha` is then `0.0` at the extract below and the predicted
    /// value is arithmetic rather than a range. A `0.016` there (the obvious "one
    /// frame") leaves `alpha == 0.32` and moves the answer to `0.19`, which reads as
    /// a broken ease.
    #[test]
    fn a_pickup_draws_the_item_in_flight_toward_its_collector() {
        const COLLECTOR: i32 = 2;
        const ITEM: i32 = 1;
        let collector_feet = Vec3::new(4.0, 0.0, 0.0);
        let mut interp = EntityInterpolator::new();
        (item_snap_with(ITEM, Vec3::ZERO, Some(stone()))).apply(interp.world_mut());
        (snap(COLLECTOR, collector_feet, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        assert!(
            begin_item_pickup(interp.world_mut(), ITEM, COLLECTOR),
            "the item was tracked with a reported stack, so a pickup must start"
        );

        // One tick, with the item gone from the server's report. `forget` is
        // what makes it gone: since #36 there is no snapshot slice to omit it
        // from, so without this the item entity stays tracked and draws
        // *alongside* its own flight animation — two item draws, not one.
        forget(interp.world_mut(), ITEM);
        (snap(COLLECTOR, collector_feet, 0.0)).apply(interp.world_mut());
        interp.update(TICK);

        let draws = interp.draws();
        let flying: Vec<&EntityDraw> = draws
            .iter()
            .filter(|d| d.type_path.as_ref() == ITEM_ENTITY_TYPE_PATH)
            .collect();
        assert_eq!(
            flying.len(),
            1,
            "exactly one item draw must survive the prune — the animation's"
        );
        let draw = flying[0];
        assert_eq!(draw.item.as_ref(), Some(&stone()));
        assert_eq!(draw.id, ITEM, "the bob phase key must stay the item's own id");

        // The target: `(x, y + eyeHeight/2, z)` — `ItemPickupParticle.updatePosition`.
        let target = Vec3::new(
            collector_feet.x,
            collector_feet.y + REMOTE_COLLECTOR_EYE_HEIGHT * PICKUP_TARGET_EYE_FRACTION,
            collector_feet.z,
        );
        let fraction = draw.feet.x / target.x;
        assert!(
            (fraction - 1.0 / 9.0).abs() < 1.0e-3,
            "one tick in, the item must be 1/9 of the way to the collector \
             (quadratic), not 1/3 (linear); it is at {} of the way, feet {:?}",
            fraction,
            draw.feet
        );
        assert!(
            draw.feet.y > 0.0 && draw.feet.y < target.y,
            "the flight must rise toward the collector's midpoint {} without \
             overshooting it; y is {}",
            target.y,
            draw.feet.y
        );
    }

    /// **The executed negative control** for the gate above: with no
    /// `begin_item_pickup` call, the very same two polls leave **no** item draw at
    /// all.
    ///
    /// Without this, the positive test is satisfied by an item track that simply
    /// failed to be pruned — which is a different bug with the same symptom, and one
    /// that would make the "1/9 of the way" assertion fail for the *right* reason
    /// only by luck.
    #[test]
    fn without_a_pickup_event_the_collected_item_simply_disappears() {
        const COLLECTOR: i32 = 2;
        const ITEM: i32 = 1;
        let collector_feet = Vec3::new(4.0, 0.0, 0.0);
        let mut interp = EntityInterpolator::new();
        (item_snap_with(ITEM, Vec3::ZERO, Some(stone()))).apply(interp.world_mut());
        (snap(COLLECTOR, collector_feet, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        // Collected, so the server stops reporting it — but *no* pickup event is
        // raised, which is the whole point of this control. `forget` is what
        // "stops reporting" means since #36; without it this asserts against an
        // unpruned track and fails for the very reason the doc above warns about.
        forget(interp.world_mut(), ITEM);
        (snap(COLLECTOR, collector_feet, 0.0)).apply(interp.world_mut());
        interp.update(TICK);
        assert!(
            !interp
                .draws()
                .iter()
                .any(|d| d.type_path.as_ref() == ITEM_ENTITY_TYPE_PATH),
            "the control must draw no item at all — otherwise the positive gate is \
             measuring an unpruned track, not an animation"
        );
    }

    /// `ItemPickupParticle.tick()` removes the particle when `life` reaches
    /// `LIFE_TIME == 3`, so the flight lasts exactly three ticks (150 ms) and then
    /// nothing is drawn. An animation that never expires leaves a copy of every item
    /// you have ever picked up hovering at your waist.
    #[test]
    fn a_pickup_animation_expires_after_exactly_three_ticks() {
        const COLLECTOR: i32 = 2;
        const ITEM: i32 = 1;
        let collector_feet = Vec3::new(4.0, 0.0, 0.0);
        let mut interp = EntityInterpolator::new();
        (item_snap_with(ITEM, Vec3::ZERO, Some(stone()))).apply(interp.world_mut());
        (snap(COLLECTOR, collector_feet, 0.0)).apply(interp.world_mut());
        interp.update(0.0);
        assert!(begin_item_pickup(interp.world_mut(), ITEM, COLLECTOR));

        // The server stops reporting the item the moment it is collected. Since
        // #36 that has to be said explicitly — otherwise the item track survives
        // every tick and this measures an unpruned track rather than the
        // animation's own three-tick life, reading `[2, 2, 1, 1, 1]`.
        forget(interp.world_mut(), ITEM);

        let mut drawn = Vec::new();
        for _ in 0..5 {
            (snap(COLLECTOR, collector_feet, 0.0)).apply(interp.world_mut());
            interp.update(TICK);
            drawn.push(
                interp
                    .draws()
                    .iter()
                    .filter(|d| d.type_path.as_ref() == ITEM_ENTITY_TYPE_PATH)
                    .count(),
            );
        }
        assert_eq!(
            drawn,
            vec![1, 1, 0, 0, 0],
            "the flight must be drawn on ticks 1 and 2 and be gone on tick 3 \
             (`life == LIFE_TIME` removes it before that tick's extract)"
        );
        assert!(interp.world().resource::<PickupAnimations>().is_empty());
    }

    /// A pickup for an item the render side never knew about starts nothing, rather
    /// than animating from a made-up position.
    ///
    /// Both halves are needed and they fail differently: an untracked *id* has no
    /// start point, and a tracked item with **no reported stack** has no model to
    /// draw. The second is the common case — `Reported::Unreported` is what a drop
    /// looks like until its `ITEM_STACK` metadata arrives.
    #[test]
    fn a_pickup_for_an_unknown_or_stackless_item_starts_nothing() {
        let mut interp = EntityInterpolator::new();
        (item_snap(7, Vec3::ZERO)).apply(interp.world_mut());
        interp.update(0.0);
        assert!(
            !begin_item_pickup(interp.world_mut(), 7, 2),
            "a tracked item with no reported stack has no model to fly"
        );
        assert!(
            !begin_item_pickup(interp.world_mut(), 999, 2),
            "an id with no track at all has no start point"
        );
        assert!(interp.world().resource::<PickupAnimations>().is_empty());
    }

    /// A pickup whose collector cannot be resolved draws nothing — and, critically,
    /// **does not panic and does not leak**: the animation still ages out on
    /// schedule.
    ///
    /// This is the live case where a mob picks something up just as it leaves view
    /// distance, and it is also the shape of the local-player fallback: if
    /// `collector_target`'s second lookup were removed, *every* pickup the player
    /// makes would land here silently.
    #[test]
    fn a_pickup_with_no_resolvable_collector_draws_nothing_and_still_expires() {
        let mut interp = EntityInterpolator::new();
        (item_snap_with(1, Vec3::ZERO, Some(stone()))).apply(interp.world_mut());
        interp.update(0.0);
        assert!(begin_item_pickup(interp.world_mut(), 1, 4242));
        forget_all(interp.world_mut());
        interp.update(TICK);
        assert!(
            !interp
                .draws()
                .iter()
                .any(|d| d.type_path.as_ref() == ITEM_ENTITY_TYPE_PATH),
            "an unresolvable collector must draw nothing rather than aim at the origin"
        );
        for _ in 0..3 {
            forget_all(interp.world_mut());
            interp.update(TICK);
        }
        assert!(
            interp.world().resource::<PickupAnimations>().is_empty(),
            "the animation must still expire, or an out-of-range collector leaks one \
             entry per pickup for the whole session"
        );
    }

    // -----------------------------------------------------------------------
    // `riding_render_seat` — the local player's per-frame seat while riding
    // -----------------------------------------------------------------------

    /// A minimal [`lodestone_model::VersionAdapter`] that answers exactly one
    /// question — the vehicle's base box height — mirroring
    /// `lodestone_ecs::player`'s own `HeightOnlyAdapter` test double (that one
    /// cannot be reused here: it is private to a different crate).
    #[derive(Debug)]
    struct SeatHeightAdapter {
        height: f32,
    }

    impl lodestone_model::VersionAdapter for SeatHeightAdapter {
        fn protocol_version(&self) -> i32 {
            0
        }

        fn minecraft_versions(&self) -> &'static [&'static str] {
            &[]
        }

        fn supports(&self, _protocol: i32) -> bool {
            false
        }

        fn entity_facts(
            &self,
            _entity_type: &lodestone_model::ResourceKey,
        ) -> Option<lodestone_model::EntityFacts> {
            Some(lodestone_model::EntityFacts {
                dimensions: lodestone_model::EntityBaseDimensions {
                    width: 1.375,
                    height: self.height,
                },
                pushes_players: false,
            })
        }

        fn begin_login(
            &self,
            _profile: &lodestone_model::LoginProfile,
            _server: &lodestone_model::ServerAddress,
        ) -> Result<Vec<lodestone_model::Directive>, lodestone_model::AdapterError> {
            unreachable!("SeatHeightAdapter answers entity_facts only")
        }

        fn handle_packet(
            &self,
            _world: &mut dyn lodestone_model::WorldSink,
            _state: lodestone_model::ConnectionState,
            _packet_id: i32,
            _payload: &[u8],
        ) -> Result<Vec<lodestone_model::Directive>, lodestone_model::AdapterError> {
            unreachable!("SeatHeightAdapter answers entity_facts only")
        }

        fn encode_action(
            &self,
            _state: lodestone_model::ConnectionState,
            _action: &lodestone_model::ClientAction,
        ) -> Result<Option<(i32, Vec<u8>)>, lodestone_model::AdapterError> {
            unreachable!("SeatHeightAdapter answers entity_facts only")
        }
    }

    const RIDING_VEHICLE_ID: i32 = 42;
    const RIDING_OWN_ID: i32 = 7;
    /// A plain boat's real box height (`EntityTypes`' boat block,
    /// `sized(1.375F, 0.5625F)`) — the same constant
    /// `lodestone_ecs::riding`'s own `a_raft_seats_higher_than_a_boat_of_the_same_box`
    /// test cites.
    const RIDING_BOAT_HEIGHT: f32 = 0.5625;

    /// A world with a tracked `"oak_boat"` vehicle carrying an interpolation
    /// track (`InterpFrom` at `from_x`, `InterpTo` at `to_x`, both `y = 64`,
    /// `z = 0`, no yaw change) and a `VersionData` that answers its height —
    /// everything [`riding_render_seat`] needs, built the way
    /// `extract_entity_draws`'s own `tracks` query expects a vehicle to look.
    fn world_with_boat_track(from_x: f32, to_x: f32, clock_t: f32) -> World {
        let mut world = World::new();
        world.insert_resource(EntityIndex::default());
        world.insert_resource(lodestone_ecs::VersionData(Some(Box::new(SeatHeightAdapter {
            height: RIDING_BOAT_HEIGHT,
        }))));
        let vehicle = world
            .spawn((
                lodestone_ecs::entity::EntityKind(
                    "oak_boat".parse().expect("valid entity type key"),
                ),
                InterpFrom {
                    feet: Vec3::new(from_x, 64.0, 0.0),
                    yaw: 0.0,
                    head_yaw: 0.0,
                    pitch: 0.0,
                },
                InterpTo {
                    feet: Vec3::new(to_x, 64.0, 0.0),
                    yaw: 0.0,
                    head_yaw: 0.0,
                    pitch: 0.0,
                },
                InterpClock {
                    t: clock_t,
                    age: 0.0,
                    window: INTERP_WINDOW,
                },
                lodestone_ecs::entity::Passengers(vec![RIDING_OWN_ID]),
            ))
            .id();
        world
            .resource_mut::<EntityIndex>()
            .insert(RIDING_VEHICLE_ID, vehicle);
        world
    }

    /// **The core claim of the fix**: the seat tracks the vehicle's own
    /// per-frame *eased* position, not its raw tick-boundary target — so a
    /// vehicle still mid-ease (`clock.t` short of the full window) seats the
    /// player short of the target too, exactly matching wherever the vehicle
    /// itself is drawn this frame.
    ///
    /// Both hypotheses are predicted and both are checked: the right one
    /// (`render_feet`'s halfway point) and the wrong one this bug actually
    /// shipped (`InterpTo.feet`, the un-eased target) — so this fails if the
    /// fix regresses back to reading the raw target, not just if the seat
    /// moves at all.
    #[test]
    fn the_seat_tracks_the_vehicles_eased_position_not_its_raw_target() {
        // Halfway through the ease: `alpha(clock) == 0.5`, so the vehicle is
        // drawn at x = (0 + 10) / 2 = 5.0 this frame, not at its x = 10.0
        // target.
        let world = world_with_boat_track(0.0, 10.0, INTERP_WINDOW * 0.5);
        let seat = riding_render_seat(&world, RIDING_VEHICLE_ID, Some(RIDING_OWN_ID))
            .expect("every link is present");

        // The right hypothesis: seated on the vehicle's eased x.
        assert!(
            (seat.x - 5.0).abs() < 1e-4,
            "seat.x was {}, want ~5.0 (the eased position, alpha 0.5 between 0 and 10)",
            seat.x
        );
        // The wrong hypothesis this bug shipped: seated on the raw target.
        assert!(
            (seat.x - 10.0).abs() > 4.0,
            "seat.x was {} — indistinguishable from the un-eased InterpTo target (10.0), \
             which is the exact regression this test exists to catch",
            seat.x
        );

        // The seat height: `Boat.rideHeight` = height / 3 = 0.5625 / 3 =
        // 0.1875, minus the player's own 0.6 vehicle attachment
        // (`riding::PLAYER_VEHICLE_ATTACHMENT_Y`) — `lodestone_ecs::riding`'s
        // own arithmetic, cited rather than restated.
        let expected_y = 64.0 + RIDING_BOAT_HEIGHT / 3.0 - 0.6;
        assert!(
            (seat.y - expected_y).abs() < 1e-4,
            "seat.y was {}, want {expected_y}",
            seat.y
        );
    }

    /// At a tick boundary (`clock.t == 0`, freshly re-anchored) the eased
    /// position collapses onto `InterpFrom`, which is where a fresh report
    /// re-anchors it to the *previously drawn* position — so this is also the
    /// frame every existing (pre-fix) tick-level gate would have looked
    /// identical to a per-frame one, per `CLAUDE.md`'s note that tick-aligned
    /// sampling is exactly where two interpolation tracks coincide.
    #[test]
    fn at_a_fresh_reanchor_the_seat_sits_at_the_old_drawn_position() {
        let world = world_with_boat_track(3.0, 10.0, 0.0);
        let seat = riding_render_seat(&world, RIDING_VEHICLE_ID, Some(RIDING_OWN_ID))
            .expect("every link is present");
        assert!(
            (seat.x - 3.0).abs() < 1e-4,
            "seat.x was {}, want 3.0 (InterpFrom, at clock.t == 0)",
            seat.x
        );
    }

    /// Every "decline rather than guess" case
    /// [`riding_render_seat`]'s own doc lists, checked directly rather than
    /// only through the positive test's absence.
    #[test]
    fn riding_render_seat_declines_rather_than_guesses() {
        // Not riding anything this client has ever heard of: no `EntityIndex`
        // entry for the vehicle id at all.
        let not_tracked = World::new();
        assert!(
            riding_render_seat(&not_tracked, RIDING_VEHICLE_ID, Some(RIDING_OWN_ID)).is_none(),
            "an untracked vehicle id must not be guessed at"
        );

        // Tracked, but no `VersionData` — the adapter that would answer the
        // vehicle's height is simply absent (e.g. before login).
        let mut no_version = World::new();
        no_version.insert_resource(EntityIndex::default());
        let vehicle = no_version
            .spawn((
                lodestone_ecs::entity::EntityKind(
                    "oak_boat".parse().expect("valid entity type key"),
                ),
                InterpFrom {
                    feet: Vec3::ZERO,
                    yaw: 0.0,
                    head_yaw: 0.0,
                    pitch: 0.0,
                },
                InterpTo {
                    feet: Vec3::ZERO,
                    yaw: 0.0,
                    head_yaw: 0.0,
                    pitch: 0.0,
                },
                InterpClock {
                    t: 0.0,
                    age: 0.0,
                    window: INTERP_WINDOW,
                },
            ))
            .id();
        no_version
            .resource_mut::<EntityIndex>()
            .insert(RIDING_VEHICLE_ID, vehicle);
        assert!(
            riding_render_seat(&no_version, RIDING_VEHICLE_ID, Some(RIDING_OWN_ID)).is_none(),
            "no VersionData means no real height to seat against, and must not fabricate one"
        );

        // Tracked, `VersionData` present, but the vehicle's interpolation
        // track has not been inserted yet (spawned this frame, `spawn_track`
        // has not run) — a real gap `extract_entity_draws` cannot hit
        // (`InterpFrom`/`InterpTo`/`InterpClock` are inserted atomically with
        // `MinecraftEntityId` by `spawn_track`) but the caller can, the one
        // frame the vehicle's `AddEntity` has arrived and its own render
        // track has not spawned yet.
        let mut no_track = World::new();
        no_track.insert_resource(EntityIndex::default());
        no_track.insert_resource(lodestone_ecs::VersionData(Some(Box::new(SeatHeightAdapter {
            height: RIDING_BOAT_HEIGHT,
        }))));
        let bare_vehicle = no_track
            .spawn(lodestone_ecs::entity::EntityKind(
                "oak_boat".parse().expect("valid entity type key"),
            ))
            .id();
        no_track
            .resource_mut::<EntityIndex>()
            .insert(RIDING_VEHICLE_ID, bare_vehicle);
        assert!(
            riding_render_seat(&no_track, RIDING_VEHICLE_ID, Some(RIDING_OWN_ID)).is_none(),
            "a vehicle with no interpolation track yet must not be guessed at"
        );
    }

    /// A seat index past the end of `Passengers` — or `Passengers` absent
    /// entirely — must fall back to seat 0 rather than panicking, the same
    /// degenerate-case-agrees contract `pin_passenger_to_vehicle` documents.
    #[test]
    fn an_unresolvable_seat_index_falls_back_to_seat_zero() {
        let world = world_with_boat_track(0.0, 0.0, 0.0);
        // `RIDING_OWN_ID` is not in this vehicle's `Passengers([RIDING_OWN_ID])`
        // list under a *different* id, so the lookup misses and must default
        // to 0 rather than panicking or guessing a later seat.
        let seat = riding_render_seat(&world, RIDING_VEHICLE_ID, Some(9999))
            .expect("every link is still present; only the seat lookup misses");
        let expected_y = 64.0 + RIDING_BOAT_HEIGHT / 3.0 - 0.6;
        assert!(
            (seat.y - expected_y).abs() < 1e-4,
            "an unresolved seat index must land on seat 0's height, got {}",
            seat.y
        );
    }

    /// [`interp_window_for`] narrows to one tick for exactly the id
    /// [`lodestone_ecs::vehicle::ControlledVehicle`] names, and falls back to
    /// the network window both when the resource is absent (every hermetic
    /// harness in this module, and the live GPU gates driving
    /// [`EntityInterpolator`] directly) and when it is present but naming a
    /// *different* vehicle (on foot, or riding something else).
    #[test]
    fn interp_window_narrows_only_for_the_vehicle_we_are_driving() {
        let mut world = World::new();
        assert!(
            (interp_window_for(&world, RIDING_VEHICLE_ID) - INTERP_WINDOW).abs() < 1e-6,
            "no ControlledVehicle resource at all must fall back to the network window"
        );

        world.insert_resource(lodestone_ecs::vehicle::ControlledVehicle(None));
        assert!(
            (interp_window_for(&world, RIDING_VEHICLE_ID) - INTERP_WINDOW).abs() < 1e-6,
            "ControlledVehicle present but None (on foot) must fall back to the network window"
        );

        let held = lodestone_ecs::vehicle::ControlledVehicleState {
            server_id: RIDING_VEHICLE_ID,
            family: lodestone_ecs::vehicle::VehicleFamily::Boat,
            motion: lodestone_physics::EntityMotion::at(lodestone_physics::Vec3d::new(0.0, 64.0, 0.0)),
            yaw: 0.0,
            pitch: 0.0,
            boat: lodestone_physics::vehicle::BoatState::default(),
            paddles: (false, false),
        };
        world.insert_resource(lodestone_ecs::vehicle::ControlledVehicle(Some(held)));
        assert!(
            (interp_window_for(&world, RIDING_VEHICLE_ID) - TICK).abs() < 1e-6,
            "the id ControlledVehicle actually names must narrow to one tick"
        );
        assert!(
            (interp_window_for(&world, RIDING_VEHICLE_ID + 1) - INTERP_WINDOW).abs() < 1e-6,
            "a *different* vehicle id (a boat we are not driving) must keep the network window"
        );
    }

    /// **The bug and the fix, both predicted, off the real fold functions.**
    ///
    /// Simulates the boat's own reported position advancing by a fixed amount
    /// every tick — exactly what `lodestone_ecs::vehicle::tick_controlled_vehicle`
    /// does to the vehicle's `Position`: a deterministic, zero-jitter local
    /// write, once per physics tick — and folds it through [`update_track`]
    /// once per tick with one real tick of `advance_interp_clocks`-equivalent
    /// time passing before the next re-anchor, exactly the cadence
    /// `Sim::step`'s per-frame `poll_net`/`fold_entities` call has relative to
    /// the `GameTick` loop.
    ///
    /// The wrong hypothesis this bug actually shipped (no `ControlledVehicle`
    /// resource in scope, so [`interp_window_for`] cannot narrow anything and
    /// every re-anchor uses the three-tick network window on a source with no
    /// jitter to smooth) must leave the drawn position — and therefore the
    /// seat [`riding_render_seat`] reads off it — trailing the true position
    /// by a real, non-vanishing gap under sustained motion. The fix (the
    /// resource naming this exact vehicle, narrowing the window to one
    /// [`TICK`]) must collapse that gap to (near) zero every tick, since
    /// `alpha` then reaches `1.0` in exactly the time before the next
    /// re-anchor.
    #[test]
    fn the_controlled_vehicles_ease_window_eliminates_the_steady_state_lag() {
        const PER_TICK_DELTA: f32 = 0.4; // ~8 blocks/s forward, well inside a boat's range
        const TICKS: i32 = 40; // long enough to reach steady state under either window

        fn base_facts() -> EntityFacts {
            let mut world = World::new();
            let entity = world
                .spawn((
                    EntityKind("oak_boat".parse().expect("valid entity type key")),
                    Position(to_model_vec3(Vec3::new(0.0, 64.0, 0.0))),
                    Rotation(lodestone_model::Rotation { yaw: 0.0, pitch: 0.0 }),
                    HeadYaw(0.0),
                    OnGround(true),
                ))
                .id();
            facts_for(&world, entity, &lodestone_game::tablist::TabList::new())
        }

        /// Spawns `base`'s track, then folds `TICKS` snapshots through
        /// [`update_track`] with `x` advancing [`PER_TICK_DELTA`] each time,
        /// advancing the clock by one [`TICK`] of real time between each fold
        /// (the same formula [`advance_interp_clocks`] uses). Returns the true
        /// target `x` and the drawn `x` after the last tick.
        fn drive(world: &mut World, base: &EntityFacts) -> (f32, f32) {
            world.insert_resource(TrackIndex::default());
            spawn_track(world, base);
            let track = world.resource::<TrackIndex>().0[&base.id];
            let mut target_x = base.feet.x;
            let mut drawn_x = base.feet.x;
            for _ in 0..TICKS {
                target_x += PER_TICK_DELTA;
                let snap = EntityFacts {
                    feet: Vec3::new(target_x, base.feet.y, base.feet.z),
                    ..base.clone()
                };
                update_track(world, track, &snap);
                {
                    let mut clock = world.get_mut::<InterpClock>(track).unwrap();
                    clock.t = (clock.t + TICK).min(clock.window);
                }
                let (from, to, clock) = (
                    *world.get::<InterpFrom>(track).unwrap(),
                    *world.get::<InterpTo>(track).unwrap(),
                    *world.get::<InterpClock>(track).unwrap(),
                );
                drawn_x = render_feet(&from, &to, &clock).x;
            }
            (target_x, drawn_x)
        }

        let base = base_facts();

        // The wrong hypothesis this bug shipped.
        let mut laggy = World::new();
        let (target, drawn) = drive(&mut laggy, &base);
        let laggy_gap = target - drawn;
        assert!(
            laggy_gap > PER_TICK_DELTA * 0.5,
            "the network window on a zero-jitter local source must leave a real steady-state \
             gap under sustained motion, got {laggy_gap} (one tick's own travel is {PER_TICK_DELTA})"
        );

        // The fix.
        let mut fixed = World::new();
        let held = lodestone_ecs::vehicle::ControlledVehicleState {
            server_id: base.id,
            family: lodestone_ecs::vehicle::VehicleFamily::Boat,
            motion: lodestone_physics::EntityMotion::at(lodestone_physics::Vec3d::new(
                f64::from(base.feet.x),
                f64::from(base.feet.y),
                f64::from(base.feet.z),
            )),
            yaw: 0.0,
            pitch: 0.0,
            boat: lodestone_physics::vehicle::BoatState::default(),
            paddles: (false, false),
        };
        fixed.insert_resource(lodestone_ecs::vehicle::ControlledVehicle(Some(held)));
        let (target, drawn) = drive(&mut fixed, &base);
        let fixed_gap = (target - drawn).abs();
        assert!(
            fixed_gap < 1.0e-4,
            "one tick's own window must let the drawn position reach the tick-boundary target \
             every single tick, got a residual gap of {fixed_gap}"
        );
    }

    /// The **discriminating** gate for "every player without a custom skin is
    /// Steve or Alex": two uuids that vanilla's hash sends to two *different*
    /// built-in identities must produce two different sheet references, and
    /// neither may be one of the two legacy names.
    ///
    /// A gate asserting only "a sheet was chosen" cannot see the bug this
    /// guards. `DefaultPlayerSkin`'s pick was already being made — its `.model`
    /// (the rig) was read and honoured — and only its `.texture` was dropped,
    /// so *some* plausible answer came back for every player. The wrong
    /// hypothesis and the right one agree on the rig and differ only here.
    ///
    /// The expected values come from vanilla's own `DEFAULT_SKINS` order and
    /// `Math.floorMod(profileId.hashCode(), 18)`, hand-evaluated: a uuid built
    /// from a small `u128` has a zero high half, so its Java `hashCode` is the
    /// low half itself and the index is simply `n % 18`. Index 1 is
    /// `slim/ari`, index 11 is `wide/efe` — chosen because they differ in
    /// *both* the identity and the rig, and because neither is `steve` or
    /// `alex`, which is exactly the collapse being guarded against.
    #[test]
    fn a_skinless_player_draws_its_uuid_hash_identity_not_steve_or_alex() {
        let tabs = lodestone_game::tablist::TabList::new();

        let ari = player_skin_for_uuid(uuid::Uuid::from_u128(1), &tabs);
        let efe = player_skin_for_uuid(uuid::Uuid::from_u128(11), &tabs);

        assert_eq!(
            ari.default_sheet, "entity/player/slim/ari",
            "uuid 1 hashes to DEFAULT_SKINS[1]"
        );
        assert_eq!(
            efe.default_sheet, "entity/player/wide/efe",
            "uuid 11 hashes to DEFAULT_SKINS[11]"
        );
        assert_ne!(
            ari.default_sheet, efe.default_sheet,
            "two uuids landing on two identities must not collapse onto one sheet"
        );
        // The collapse itself, named: before the sheet was carried, both of
        // these drew the pack's plain rig sheet and the other sixteen
        // identities were unreachable.
        for skin in [&ari, &efe] {
            assert!(
                !skin.default_sheet.ends_with("/steve") && !skin.default_sheet.ends_with("/alex"),
                "{} is a legacy identity -- the hash pick has collapsed",
                skin.default_sheet
            );
        }
        // And the rig still tracks the identity's own half of the array.
        assert!(ari.model.is_slim(), "DEFAULT_SKINS[1] is in the slim half");
        assert!(!efe.model.is_slim(), "DEFAULT_SKINS[11] is in the wide half");
    }

}
