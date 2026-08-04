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
//! system — [`fold_snapshots`], whose input is a borrowed
//! `&[EntitySnapshot]` slice `sim.rs` owns; read its own docs before moving it.
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
//! needlessly — see [`EntitySnapshot::on_ground`].
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
    Component, Entity, IntoScheduleConfigs, Query, Res, ResMut, Resource, With,
};
use bevy_ecs::world::World;
use glam::Vec3;
use lodestone_assets::ResourceLocation;
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::entity::{
    AttackSwing, EntityIndex, HurtTime, ItemUse, MinecraftEntityId, MobState,
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
use lodestone_physics::{
    CollisionView, EntityDimensions, EntityMotion, MoveContext, PhysicsProfile, Vec3d, mth,
    move_entity,
};
use lodestone_render::{AnimInput, ArmPose, mob_draws_bow_when_aggressive};

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

/// A version-free entity snapshot as reported by the client for one tick. Built
/// by the sim from an [`EntityView`](lodestone_client::EntityView); carries only
/// what the renderer needs, in glam types, so this module needs no client or
/// model dependency.
///
/// # Slated for deletion
///
/// `docs/bevy-migration.md` Stage 1 deletes this type: it is the second of the
/// three entity-pose copies (`EntityView` → `EntitySnapshot` → the render
/// components), and once ingest writes the render components directly there is
/// nothing left for it to carry. It survives today only because its producer
/// lives in `net.rs` and its consumer in `sim.rs` — see [`fold_snapshots`].
/// A resolved nametag (issue #100): the plain text to draw above the entity,
/// plus whether the depth-see-through pass applies.
///
/// Resolved once, at the `net::entity_snapshot` boundary — a player's tag
/// from the tab list, every other entity's from its `CUSTOM_NAME`/
/// `CUSTOM_NAME_VISIBLE` metadata — so [`EntitySnapshot`], [`EntityDraw`] and
/// the nametag pass never need to know the two rules differ. See
/// `crate::net::entity_snapshot`'s doc for the exact vanilla predicates
/// (jar file:line) and `docs/entity-nametags.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameTag {
    /// The plain string to draw (already flattened through
    /// [`lodestone_model::Text::to_plain_string`] — a translation-key custom
    /// name, the rare case, draws its raw key rather than resolving through
    /// the shell's chat language table; see `docs/entity-nametags.md`).
    pub text: String,
    /// Whether the depth-testless, faded pass draws in addition to the normal
    /// depth-tested one — `false` while the entity is sneaking
    /// (`Entity.isDiscrete()`), which is when vanilla suppresses it. See
    /// `gpu/nametag.rs`'s module doc for the two passes' exact depth
    /// settings.
    pub see_through: bool,
}

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
    /// Exactly the shape the read-model's own field is: [`Reported::Unreported`]
    /// is "the server has never reported a stack for this entity",
    /// [`Reported::Reported(None)`](Reported::Reported) is an explicitly
    /// *empty* stack. `Unreported` therefore means "unknown", and
    /// [`fold_snapshots`] leaves any previously recorded stack alone rather than
    /// clearing it — a drop names itself once and then goes quiet, so treating
    /// silence as "empty" would blank it a frame later.
    ///
    /// This is a [`ResourceLocation`], not a model `ItemStack`: `EntitySnapshot`
    /// is deliberately model-free, and the renderer only ever needs the item
    /// *id* to pick a model. The stack's `count` and data components are dropped
    /// at the boundary that builds this (`net::entity_snapshot`) — see the note
    /// there, since count is visible in vanilla.
    pub item: Reported<ResourceLocation>,
    /// The entity's last-reported velocity in blocks per tick
    /// (`set_entity_motion`/`add_entity`), when the server has ever sent one.
    ///
    /// This is what the [`ItemPhysics`] component seeds and re-anchors its
    /// ballistic simulation from — see the module docs on why a dropped item
    /// needs real physics rather than a position ease. `None` is "never
    /// reported", not "zero"; a zero velocity is reported as `Some(Vec3::ZERO)`.
    pub velocity: Option<Vec3>,
    /// Whether the server last reported this entity resting on the ground
    /// (`on_ground` on `add_entity`/`teleport_entity`/`move_entity`).
    ///
    /// [`tick_item_physics`] pauses its simulation while this is `true`, because
    /// a resting item does not need resimulating.
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
    /// (`lodestone_ecs::ingest`'s `apply_entity_equipment` merges each update
    /// into the `Equipment` component and never clears), so every snapshot
    /// carries the complete current set and [`fold_snapshots`] replaces its
    /// record wholesale — unlike [`Self::item`], which arrives once and must
    /// never be cleared by silence.
    ///
    /// Armour slots reach a pixel too — `RenderState`'s `prepare_armour` walks
    /// `ArmourSlot::ALL` against this same list.
    pub equipment: Vec<(EquipmentSlot, Option<ResourceLocation>)>,
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
    pub equipment_dye: Vec<(EquipmentSlot, u32)>,
    /// The entity's decoded cosmetic variant (sheep dye/shear, villager type,
    /// horse markings, …), as last reported.
    ///
    /// Exactly [`EntityView::variant`](lodestone_client::EntityView::variant)'s
    /// own contract, copied through verbatim like [`Self::equipment`]: `None`
    /// means the server has never reported an override, which is a different
    /// state from a known-but-default variant. There is no "explicitly
    /// cleared" state to preserve here — vanilla never un-reports a variant —
    /// so unlike [`Self::item`] this needs no [`Reported`] wrapper.
    ///
    /// Only [`EntityVariant::Dyed`] reaches a pixel today, and only when
    /// [`Self::type_path`] is `"sheep"` — see [`EntityDraw::wool`] and
    /// `docs/entity-rendering.md`'s "Render layers: sheep wool" section.
    pub variant: Option<EntityVariant>,
    /// How many items the stack named by [`Self::item`] represents, as last
    /// reported. Meaningless when [`Self::item`] is [`Reported::Unreported`]
    /// or an explicit empty stack; `1` in both of those cases, and whenever
    /// [`Self::type_path`] is not [`ITEM_ENTITY_TYPE_PATH`].
    ///
    /// This was dropped at the `net::entity_snapshot` boundary along with the
    /// rest of the stack's data components; unlike those, it changes *how
    /// many* copies vanilla draws rather than how one looks
    /// (`ItemClusterRenderState::getRenderedAmount`: 1 copy at count ≤ 1, then
    /// 2, 3, 4, 5 as the count passes 1, 16, 32 and 48), so restoring it needed
    /// only this plain `u32` — see [`EntityDraw::count`] for where the
    /// multi-copy draw itself still needs wiring.
    pub count: u32,
    /// This entity's resolved nametag (issue #100), or `None` when nothing
    /// should draw above it — a mob with no visible custom name, or a player
    /// entity with no matching tab-list entry. See [`NameTag`].
    pub name_tag: Option<NameTag>,
    /// A creeper's synced fuse direction (`Creeper.DATA_SWELL_DIR`), as last
    /// reported — meaningless when [`Self::type_path`] is not `"creeper"`.
    ///
    /// `None` means "the server has never reported this", exactly
    /// [`Self::variant`]'s own contract: [`spawn_track`] seeds a fresh
    /// creeper's [`CreeperFuse`] from vanilla's own idle default
    /// ([`CreeperFuse::IDLE`]) rather than treating `None` as zero, and
    /// [`update_track`] only overwrites the direction when a snapshot
    /// actually carries one — silence must not reset a mid-fuse creeper back
    /// to idle.
    pub creeper_swell_dir: Option<i32>,
}

/// A sheep's decoded wool state, narrowed from [`EntitySnapshot::variant`] for
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
    /// [`EntitySnapshot::equipment_dye`] narrowed the same way `equipment`
    /// narrows [`EntitySnapshot::equipment`] — see that field's doc for why
    /// this is additive rather than folded into `equipment`'s own tuple.
    pub equipment_dye: Vec<(EquipmentSlot, u32)>,
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
    /// Meaningless (and left at the neutral `1`) for every entity that is not
    /// a dropped item with a known stack.
    ///
    /// **Not yet drawn as more than one copy.** Vanilla's
    /// `ItemClusterRenderState::getRenderedAmount` turns this into 1–5 jittered
    /// copies (`prepare_item_geometry` in `gpu.rs` still draws exactly one
    /// regardless of `count`) — see `docs/dropped-items.md`. This field is the
    /// data half of that gap; the draw half is a specified, unlanded patch to a
    /// held file.
    pub count: u32,
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
    /// This entity's resolved nametag (issue #100), narrowed from
    /// [`RenderNameTag`]. `None` draws nothing — the common case for every
    /// entity with no visible custom name.
    pub name_tag: Option<NameTag>,
    /// Whether the hurt/death **red overlay** applies to this entity's model
    /// this frame — vanilla's `state.hasRedOverlay = entity.hurtTime > 0 ||
    /// entity.deathTime > 0` (`LivingEntityRenderer.java:281`), issue #98.
    ///
    /// Boolean, not a fade: vanilla does not interpolate by how much of
    /// `hurtTime` remains, so neither does this (see
    /// [`lodestone_render::EntityInstanceRaw::with_hurt_overlay`]).
    ///
    /// Read off [`lodestone_ecs::entity::HurtTime`] through [`EntityIndex`] in
    /// [`extract_entity_draws`], **not** folded through [`EntitySnapshot`] —
    /// exactly like [`Self::anim`]'s `attack_anim`, and for the same two
    /// reasons: the component lives on the *ingest* entity rather than the
    /// render one, and `EntitySnapshot` is the copy `docs/bevy-migration.md`
    /// Stage 1 deletes. `docs/combat.md`'s original patch spec called for an
    /// `EntitySnapshot::hurt` field instead; that would have added a third
    /// hop and rippled through ~15 struct literals for a value the extract
    /// system can already reach directly.
    ///
    /// `deathTime` has no component on this side of the wire — vanilla's death
    /// animation is not decoded — so today this is `hurtTime > 0` alone. That
    /// makes the overlay end ~10 ticks after the killing blow instead of
    /// persisting through the fall-over, which is the only visible divergence.
    pub hurt: bool,
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
}

// ---------------------------------------------------------------------------
// The render-side component set
// ---------------------------------------------------------------------------

/// The entity type's canonical path, as the snapshot reported it.
///
/// Distinct from `lodestone_ecs::entity::EntityKind` (a `ResourceKey`) only
/// because [`EntitySnapshot`] speaks the bare path string that
/// `lodestone-render`'s model set is keyed by. When `EntitySnapshot` dies these
/// two collapse into one component; until then this is the render vocabulary and
/// `EntityKind` is the network one.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct RenderKind(pub String);

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
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct InterpClock {
    /// Seconds since the ease was last re-anchored, capped at [`INTERP_WINDOW`].
    pub t: f32,
    /// Continuous age in ticks (`ageInTicks`), driving idle bob.
    pub age: f32,
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

/// Vanilla's `Creeper.DEFAULT_MAX_SWELL` (`Creeper.java:51`): the tick count a
/// fuse counts up to before [`tick_creeper_fuse`] treats it as fully swollen.
/// A creeper's real `maxSwell` can differ (the `Fuse` NBT tag), but that value
/// is never synchronised to the client — see
/// [`lodestone_render::entity_anim::MAX_SWELL`]'s doc for the same constant on
/// the render side (`/ 28`, not `/ 30`, is `maxSwell - 2`).
const CREEPER_MAX_SWELL_TICKS: i32 = 30;

/// A creeper's fuse, integrated **client-side** one tick at a time from the
/// synced [`EntitySnapshot::creeper_swell_dir`] — exactly what vanilla's own
/// client does (`Creeper.java:139`, `this.swell += swellDir`), because only
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
    /// Vanilla's own accessor default (`Creeper.java:100`,
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
/// exactly one tick, byte-for-byte `Creeper.java:139`'s
/// `this.swell += swellDir` (clamped `0..=maxSwell` the same way `tick()`
/// clamps it — `Creeper.java:140-145`). Run client-side because only
/// [`CreeperFuse::swell_dir`] is ever synced; the counter itself is not.
pub fn tick_creeper_fuse(mut fuses: Query<&mut CreeperFuse>) {
    for mut fuse in &mut fuses {
        fuse.old_swell = fuse.swell;
        fuse.swell = (fuse.swell + fuse.swell_dir).clamp(0, CREEPER_MAX_SWELL_TICKS);
    }
}

/// The occupied equipment slots, narrowed from [`EntitySnapshot::equipment`].
///
/// A component rather than a side table (as [`ItemStacks`] is) precisely
/// *because* it is replaced wholesale every poll: there is no "reported once,
/// then silence" hazard to guard against, so there is nothing for a separate
/// table's prune to protect, and hanging it on the entity means a despawn prunes
/// it for free.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct RenderEquipment(pub Vec<(EquipmentSlot, ResourceLocation)>);

/// Per-slot `minecraft:dyed_color`, narrowed from
/// [`EntitySnapshot::equipment_dye`] — a separate component from
/// [`RenderEquipment`] rather than a wider tuple inside it, for the same
/// reason the snapshot field is additive; see that field's doc.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderEquipmentDye(pub Vec<(EquipmentSlot, u32)>);

/// This entity's sheep-wool state, narrowed from [`EntitySnapshot::variant`] by
/// [`sheep_wool`].
///
/// A component for the same reason [`RenderEquipment`] is one rather than a
/// side table: it is replaced wholesale every poll (shearing is a metadata
/// update, not a movement), so there is no "reported once, then silence"
/// hazard and a despawn prunes it for free.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderWool(pub Option<SheepWool>);

/// This entity's resolved nametag (issue #100), narrowed from
/// [`EntitySnapshot::name_tag`].
///
/// A component for the same reason [`RenderEquipment`]/[`RenderWool`] are:
/// replaced wholesale every poll (a player's tab-list name can change, a
/// mob's `CUSTOM_NAME_VISIBLE` can toggle, both with no movement at all), so
/// there is no "reported once, then silence" hazard and a despawn prunes it
/// for free.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderNameTag(pub Option<NameTag>);

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
/// `position.y + eyeHeight` (`Entity.java:3798`) — an **absolute** Y, not an
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
/// `fold_snapshots` next runs, the server has stopped reporting the item and the
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
/// **Must be called before the frame's `fold_snapshots`.** `Sim::poll_net` runs
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
            type_path: ITEM_ENTITY_TYPE_PATH.to_string(),
            item: Some(pickup.item.clone()),
            count: pickup.count,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            wool: None,
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
            // A flying pickup is an item entity, not a living one: nothing can be
            // using it, so its variant resolves in `DisplaySlot::Ground` alone.
            item_use: None,
            // An item entity is never a creeper.
            creeper_swelling: 0.0,
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
    (clock.t / INTERP_WINDOW).clamp(0.0, 1.0)
}

/// The currently-drawn position: [`InterpFrom`] eased toward [`InterpTo`].
fn render_feet(from: &InterpFrom, to: &InterpTo, clock: &InterpClock) -> Vec3 {
    from.feet.lerp(to.feet, alpha(clock))
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
fn render_anim(
    from: &InterpFrom,
    to: &InterpTo,
    clock: &InterpClock,
    walk: &WalkAnim,
    partial_tick: f32,
    swing_progress: f32,
    arm_pose: ArmPoseChoice,
    aggressive: bool,
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
fn arm_pose_for(
    type_path: &str,
    equipment: &[(EquipmentSlot, ResourceLocation)],
    item_use: Option<ItemUse>,
    aggressive: bool,
) -> ArmPoseChoice {
    // `AbstractSkeletonRenderer.getArmPose`'s override, ahead of the base
    // using-item rule exactly as vanilla's `? :` puts it ahead of `super`.
    //
    // `getMainArm() == arm` is vanilla's left-handed fork; a right-handed mob is
    // assumed here, so the pose always lands in the main hand. `MobFlags`
    // decodes the left-handed bit and nothing consumes it yet — see that
    // accessor's doc for why.
    if aggressive && mob_draws_bow_when_aggressive(type_path) && main_hand_holds_bow(equipment) {
        return ArmPoseChoice {
            pose: ArmPose::BowAndArrow,
            left_hand: false,
        };
    }
    let Some(item_use) = item_use else {
        return ArmPoseChoice::default();
    };
    if !item_use.using {
        return ArmPoseChoice::default();
    }
    let slot = if item_use.off_hand {
        EquipmentSlot::OffHand
    } else {
        EquipmentSlot::MainHand
    };
    let Some((_, held)) = equipment.iter().find(|(s, _)| *s == slot) else {
        // Using something we were never told about. `Empty` rather than a guess:
        // equipment and metadata are separate packets and either can arrive first.
        return ArmPoseChoice::default();
    };
    if held.namespace() != VANILLA {
        return ArmPoseChoice::default();
    }
    let pose = match held.path() {
        BOW_PATH => ArmPose::BowAndArrow,
        CROSSBOW_PATH => ArmPose::CrossbowCharge {
            progress: item_use.ticks as f32 / CROSSBOW_CHARGE_TICKS,
        },
        // Every other item's use animation (eat, drink, block, spyglass, …) is a
        // pose `ArmPose` does not model yet; see its docs.
        _ => return ArmPoseChoice::default(),
    };
    ArmPoseChoice {
        pose,
        left_hand: item_use.off_hand,
    }
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
        clock.t = (clock.t + delta.0).min(INTERP_WINDOW);
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
    // `render_anim`'s doc for why this is not folded through `EntitySnapshot`
    // like every other field here.
    index: Res<EntityIndex>,
    swings: Query<&AttackSwing>,
    // `HurtTime` lives on the ingest entity too (`apply_entity_damaged` /
    // `apply_entity_hurt_animation` resolve it through the same `EntityIndex`),
    // so it is bridged the same way `AttackSwing` is rather than folded through
    // `EntitySnapshot` — see `EntityDraw::hurt`.
    hurts: Query<&HurtTime>,
    // `ItemUse` lives on the ingest entity too (`apply_entity_item_use` resolves
    // the living-flags byte through the same `EntityIndex`), bridged the same way
    // `AttackSwing` and `HurtTime` are. It is what turns a metadata bit into a bow
    // draw — see `arm_pose_for` and issue #57.
    item_uses: Query<&ItemUse>,
    // `MobState` lives on the ingest entity too (`apply_entity_metadata` folds the
    // *mob* flags byte into it), bridged the same way. It is what turns
    // `Mob.isAggressive()` into a drawn bow and into a zombie's raised arms — see
    // `arm_pose_for` and issue #379.
    mob_states: Query<&MobState>,
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
        &RenderWool,
        &RenderNameTag,
        // `Option`, not `&CreeperFuse` bare: present only on creepers, same
        // "absence is the switch" shape `ItemPhysics` uses elsewhere in this
        // module. Every non-creeper entity matches `None` here at zero cost.
        Option<&CreeperFuse>,
    )>,
    mut out: ResMut<ExtractedDraws>,
) {
    // The one accumulator's residual, published by `FrameClock::end_frame` before
    // `Extract` runs. This used to be `TickAccum`, the interpolator `World`'s own
    // second accumulator; it is now the *same* number the camera interpolates the
    // player with, which is the point of §4.1(c).
    let partial_tick = clock.interp_alpha.clamp(0.0, 1.0);
    out.0.clear();
    for (id, kind, scale, from, to, clock, walk, equipment, equipment_dye, wool, name_tag, fuse) in
        &tracks
    {
        // One lookup, not two: `item` and `count` both come from the same
        // recorded stack, and a drop with no stack yet must not manufacture a
        // count out of nowhere.
        let stack = (kind.0 == ITEM_ENTITY_TYPE_PATH)
            .then(|| stacks.0.get(&id.0))
            .flatten();
        // `0.0` for an id with no ingest entity (shouldn't happen — a render
        // track only exists once the entity has been spawned) or one that has
        // never swung (`AttackSwing` absent, like `HurtTime`).
        let swing_progress = index
            .get(id.0)
            .and_then(|entity| swings.get(entity).ok())
            .map_or(0.0, |swing| swing.attack_anim_lerp(partial_tick));
        // `hurtTime > 0`, vanilla's `hasRedOverlay` gate. `false` for an entity
        // that has never been hit (`HurtTime` absent, like `AttackSwing`) — and
        // also for one whose countdown has aged out, since `tick_hurt_time`
        // leaves the component in place at zero rather than removing it.
        let hurt = index
            .get(id.0)
            .and_then(|entity| hurts.get(entity).ok())
            .is_some_and(|hurt| hurt.0 > 0);
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
        // One lookup, two consumers: the arm pose below and `EntityDraw::item_use`,
        // which the held-item pass resolves the item's own definition tree against.
        // Reading it twice would let the two disagree about the same tick.
        let item_use = index
            .get(id.0)
            .and_then(|entity| item_uses.get(entity).ok())
            .map(|item_use| *item_use);
        let arm_pose = arm_pose_for(&kind.0, &equipment.0, item_use, aggressive);
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
        out.0.push(EntityDraw {
            id: id.0,
            type_path: kind.0.clone(),
            item: stack.map(|s| s.id.clone()),
            count: stack.map_or(1, |s| s.count),
            equipment: equipment.0.clone(),
            equipment_dye: equipment_dye.0.clone(),
            wool: wool.0,
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
            ),
            name_tag: name_tag.0.clone(),
            hurt,
            item_use,
            creeper_swelling,
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

/// Fold this frame's [`EntitySnapshot`]s into the component set: spawn tracks
/// for newly-seen entities, re-anchor eases for ones that moved or turned, and
/// prune everything the report no longer mentions.
///
/// # Why this is not a `NetIngest` system either
///
/// Two reasons, both of which the plan expects to disappear together with
/// [`EntitySnapshot`]:
///
/// 1. **Its input is a borrowed slice from `sim.rs`.** The same `'static`
///    problem [`tick_item_physics`] used to have — but there is no
///    `CollisionSource`-shaped fix available here, because the borrow is a
///    `Vec` the caller owns, not a view an owned adapter could rebuild on
///    demand. The real fix is ingest writing these components directly
///    instead of round-tripping through that `Vec`.
/// 2. **It runs *after* the tick loop, not before it.** The plan's schedule
///    order is `NetIngest` → `GameTick`; this module's order is clocks →
///    ticks → fold, and every numeric expectation in the ~25 tests below is
///    written against it. Reordering is a behaviour change, not a refactor, so
///    it belongs in the change that also deletes `EntitySnapshot`.
fn fold_snapshots(world: &mut World, snapshots: &[EntitySnapshot]) {
    for snap in snapshots {
        // Fold the reported identity first, so a drop is never drawn for a frame
        // as a placeholder before its item lands. `Unreported` is "this snapshot
        // does not know", which must not clear what an earlier one established;
        // only an explicit empty stack clears.
        match &snap.item {
            Reported::Reported(Some(item)) => {
                world.resource_mut::<ItemStacks>().0.insert(
                    snap.id,
                    TrackedStack {
                        id: item.clone(),
                        count: snap.count,
                    },
                );
            }
            Reported::Reported(None) => {
                world.resource_mut::<ItemStacks>().0.remove(&snap.id);
            }
            Reported::Unreported => {}
        }

        match world.resource::<TrackIndex>().0.get(&snap.id).copied() {
            None => spawn_track(world, snap),
            Some(entity) => update_track(world, entity, snap),
        }
    }

    // Drop tracks for entities no longer reported — and the item stacks recorded
    // against them, or a long session leaks one entry per drop.
    let seen: HashSet<i32> = snapshots.iter().map(|s| s.id).collect();
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

/// A newly seen entity is drawn at rest at its reported pose: both ends of the
/// ease are the same, and the clock starts *finished* so nothing eases from
/// nowhere.
fn spawn_track(world: &mut World, snap: &EntitySnapshot) {
    let is_item = snap.type_path == ITEM_ENTITY_TYPE_PATH;
    let is_creeper = snap.type_path == "creeper";
    let mut entity = world.spawn((
        MinecraftEntityId(snap.id),
        RenderKind(snap.type_path.clone()),
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
            t: INTERP_WINDOW,
            age: 0.0,
        },
        WalkAnim {
            walk: WalkAnimation::new(),
            last_feet: snap.feet,
        },
        RenderEquipment(occupied_equipment(&snap.equipment)),
        RenderEquipmentDye(snap.equipment_dye.clone()),
        RenderWool(sheep_wool(&snap.type_path, snap.variant.as_ref())),
        RenderNameTag(snap.name_tag.clone()),
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
fn update_track(world: &mut World, entity: Entity, snap: &EntitySnapshot) {
    let Ok(mut entity) = world.get_entity_mut(entity) else {
        return;
    };
    let is_item = snap.type_path == ITEM_ENTITY_TYPE_PATH;

    if let Some(mut kind) = entity.get_mut::<RenderKind>() {
        kind.0.clone_from(&snap.type_path);
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
    // Same "outside the motion gate" reasoning once more: a creeper's fuse
    // direction can flip while it stands still (backing away from a player it
    // was swelling toward). Only overwritten when *reported* — `None` means
    // this packet did not mention it, which must never reset a mid-fuse
    // creeper back to idle. See `EntitySnapshot::creeper_swell_dir`.
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
/// this plugin's [`fold_snapshots`] spawns a second, render-side entity per mob
/// keyed by [`TrackIndex`], with [`EntitySnapshot`] the sanctioned intermediate
/// between them.
///
/// Collapsing *that* is Stage 1's remaining debt, and §4.1(c) has removed one of
/// its two stated blockers: [`fold_snapshots`]'s "its input is a borrowed slice
/// from `sim.rs`" no longer holds, since the components it would read are now in
/// the same `World`. The other blocker stands — ingest runs in `NetIngest`, which
/// the plan orders *before* `GameTick`, while this module's order is clocks →
/// ticks → fold and every numeric expectation in the ~25 tests below is written
/// against it.
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

/// Fold this frame's snapshots into the render-side component set — the free
/// function behind [`EntityInterpolator::update_with_view`]'s third step, for a
/// driver that owns the `World` itself.
pub fn fold_entity_snapshots(world: &mut World, snapshots: &[EntitySnapshot]) {
    fold_snapshots(world, snapshots);
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

/// Record which item a dropped-item entity is carrying, in a `World` the caller
/// owns. See [`EntityInterpolator::set_item_stack`] for the live chain.
///
/// Sets the stack count to `1` — the neutral value for a caller that only
/// knows identity. [`fold_snapshots`] does not go through this function; it
/// writes [`TrackedStack`] directly so it can carry the real reported count.
pub fn set_item_stack_in(world: &mut World, entity_id: i32, item: ResourceLocation) {
    world
        .resource_mut::<ItemStacks>()
        .0
        .insert(entity_id, TrackedStack { id: item, count: 1 });
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
    /// [`fold_snapshots`] does, from [`EntitySnapshot::item`], for every
    /// snapshot that carries a stack — the full live chain is `ITEM_STACK`
    /// metadata (index 8) → `EntityMetadataUpdate::item` →
    /// `lodestone_ecs::entity::DisplayItem` → `EntityView::item` →
    /// `net::entity_snapshot` → here. It stays a public setter because it is
    /// also the direct seam for tests and for any caller that learns an item's
    /// identity outside the snapshot stream.
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
    /// [`fold_snapshots`] calls this (via the same path as
    /// [`Self::set_item_stack`]'s doc comment describes) with
    /// [`EntitySnapshot::count`], which `net::entity_snapshot` reads straight
    /// off the wire's `ItemStack::count` — no model dependency needed to widen
    /// this far, per `docs/dropped-items.md`.
    pub fn set_item_stack_with_count(&mut self, entity_id: i32, item: ResourceLocation, count: u32) {
        self.world
            .resource_mut::<ItemStacks>()
            .0
            .insert(entity_id, TrackedStack { id: item, count });
    }

    /// Forget the item recorded for `entity_id`, so it draws as an empty stack.
    ///
    /// Only reached when the server *explicitly* reports an empty stack; a
    /// snapshot that is merely silent about the item leaves the record alone.
    pub fn clear_item_stack(&mut self, entity_id: i32) {
        self.world.resource_mut::<ItemStacks>().0.remove(&entity_id);
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
    /// [`Self::set_item_stack_with_count`] and the live [`fold_snapshots`]
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
    pub fn update(&mut self, snapshots: &[EntitySnapshot], dt: f32) {
        self.update_with_view(
            snapshots,
            dt,
            PlayerCollision::View(Arc::new(OpenAir)),
            &PhysicsProfile::mc_1_21(),
        );
    }

    /// Advance every track by `dt` seconds, then fold in this frame's snapshots.
    ///
    /// The order is load-bearing and is what the tests below are written
    /// against:
    ///
    /// 1. [`Update`] → [`advance_interp_clocks`]: every ease clock and age moves
    ///    on, so a snapshot that resets `t` this frame anchors from the pose
    ///    that was actually on screen.
    /// 2. per 20 Hz tick: [`GameTick`] → [`tick_item_physics`] (`TickSet::Physics`)
    ///    then [`tick_walk_animation`] (`TickSet::Animate`). Both run on a fixed
    ///    clock, not per frame.
    /// 3. [`fold_snapshots`]: this frame's report, then the prune.
    /// 4. [`Extract`] → [`extract_entity_draws`], so [`Self::draws`] is a plain
    ///    read.
    ///
    /// Entities absent from `snapshots` are dropped (despawned/out of range).
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
        snapshots: &[EntitySnapshot],
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

        fold_snapshots(&mut self.world, snapshots);

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
            arm_pose_for("skeleton", &bow_in_main_hand(), None, true).pose,
            ArmPose::BowAndArrow
        );
        // Every `AbstractSkeletonRenderer` subclass, so the type set is not just
        // "skeleton" with the others assumed.
        for kind in ["wither_skeleton", "stray", "bogged", "parched"] {
            assert_eq!(
                arm_pose_for(kind, &bow_in_main_hand(), None, true).pose,
                ArmPose::BowAndArrow,
                "{kind} is drawn by AbstractSkeletonRenderer and must get the draw too"
            );
        }
        // It lands in the main hand: vanilla's `getMainArm() == arm` term, with a
        // right-handed mob assumed.
        assert!(!arm_pose_for("skeleton", &bow_in_main_hand(), None, true).left_hand);

        // Not aggressive — the flag is the trigger, and this is the case every
        // skeleton in the world was stuck in before #379.
        assert_eq!(
            arm_pose_for("skeleton", &bow_in_main_hand(), None, false).pose,
            ArmPose::Empty
        );
        // Aggressive, bow, wrong renderer family. `AbstractZombieRenderer` and
        // `IllagerRenderer` have no such override.
        for kind in ["zombie", "husk", "drowned", "pillager", "player"] {
            assert_eq!(
                arm_pose_for(kind, &bow_in_main_hand(), None, true).pose,
                ArmPose::Empty,
                "{kind} is not drawn by AbstractSkeletonRenderer and must not get the draw"
            );
        }
        // Aggressive skeleton, bow in the *off* hand: vanilla reads
        // `getMainHandItem()`, so this is the rest pose.
        let off_hand = vec![(
            EquipmentSlot::OffHand,
            "minecraft:bow".parse::<ResourceLocation>().expect("key"),
        )];
        assert_eq!(
            arm_pose_for("skeleton", &off_hand, None, true).pose,
            ArmPose::Empty
        );
        // Aggressive skeleton, no bow at all.
        assert_eq!(
            arm_pose_for("skeleton", &[], None, true).pose,
            ArmPose::Empty
        );
        // And a *non-vanilla* `bow` is a different item, not a bow.
        let modded = vec![(
            EquipmentSlot::MainHand,
            "mypack:bow".parse::<ResourceLocation>().expect("key"),
        )];
        assert_eq!(
            arm_pose_for("skeleton", &modded, None, true).pose,
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
            arm_pose_for("player", &bow_in_main_hand(), Some(drawing), false).pose,
            ArmPose::BowAndArrow,
            "a remote player drawing a bow is #57's mechanism and must be untouched"
        );
        // ...including on a skeleton, where both mechanisms could apply. Vanilla's
        // `? :` puts the aggressive branch first, but the answer is the same pose,
        // so what matters is that neither path is shadowed into never firing.
        assert_eq!(
            arm_pose_for("skeleton", &bow_in_main_hand(), Some(drawing), false).pose,
            ArmPose::BowAndArrow
        );
    }

    fn snap(id: i32, feet: Vec3, yaw: f32) -> EntitySnapshot {
        EntitySnapshot {
            id,
            type_path: "pig".into(),
            feet,
            yaw,
            head_yaw: yaw,
            pitch: 0.0,
            scale: 1.0,
            item: Reported::Unreported,
            velocity: None,
            on_ground: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            variant: None,
            count: 1,
            name_tag: None,
            creeper_swell_dir: None,
        }
    }

    fn creeper_snap(id: i32, swell_dir: Option<i32>) -> EntitySnapshot {
        EntitySnapshot {
            type_path: "creeper".into(),
            creeper_swell_dir: swell_dir,
            ..snap(id, Vec3::ZERO, 0.0)
        }
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
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], INTERP_WINDOW);
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

        interp.update(&[snap(1, Vec3::ZERO, 0.0)], 0.0);
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
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], INTERP_WINDOW);
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
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], 0.0);
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
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], 0.0);
        assert!(
            !interp.draws()[0].hurt,
            "HurtTime(0) is an expired countdown, not an absent one — the overlay must clear"
        );
    }

    /// The full creeper-swell chain this pass wired, exercised end to end
    /// through the public `update` seam — `EntitySnapshot::creeper_swell_dir`
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
        interp.update(&[snap(1, Vec3::ZERO, 0.0)], 1.0);
        assert_eq!(
            interp.draws()[0].creeper_swelling,
            0.0,
            "a non-creeper must never report a nonzero swell"
        );

        // A freshly spawned creeper whose first snapshot has not yet reported
        // a fuse direction: seeded from vanilla's own idle default (`-1`), so
        // it must read 0.0 too, the same as before this feature existed.
        interp.update(&[creeper_snap(2, None)], 0.0);
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
        interp.update(&[creeper_snap(2, Some(1))], 0.0);
        interp.update(&[creeper_snap(2, Some(1))], 1.0);
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
        interp.update(&[creeper_snap(2, Some(-1))], 0.0);
        interp.update(&[creeper_snap(2, Some(-1))], 2.0);
        let receded = draw_for(&interp, 2).creeper_swelling;
        assert!(
            receded < swelling,
            "swelling did not recede after 2s at swell_dir=-1: was {swelling}, now {receded}"
        );
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
        interp.update(&[sheep, pig], 0.016);
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
        interp.update(std::slice::from_ref(&s), 0.016);
        assert_eq!(interp.draws()[0].wool.map(|w| w.sheared), Some(false));

        // Identical pose, freshly sheared.
        s.variant = Some(EntityVariant::Dyed {
            color: 3,
            sheared: true,
        });
        interp.update(std::slice::from_ref(&s), 0.016);
        assert_eq!(
            interp.draws()[0].wool.map(|w| w.sheared),
            Some(true),
            "wool state must not be gated on movement, same as equipment"
        );
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
            item: Reported::Unreported,
            velocity: None,
            on_ground: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            variant: None,
            count: 1,
            name_tag: None,
            creeper_swell_dir: None,
        }
    }

    /// The same, carrying a reported stack, as the live path builds it.
    fn item_snap_with(id: i32, feet: Vec3, item: Option<ResourceLocation>) -> EntitySnapshot {
        EntitySnapshot {
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

    /// The stack count's own hop across the same boundary velocity/equipment
    /// crossed before it: `EntitySnapshot::count` -> `TrackedStack` ->
    /// `EntityDraw::count`, via `fold_snapshots` — the live path
    /// `net::entity_snapshot` feeds, not the setter.
    #[test]
    fn item_count_reaches_the_draw() {
        let mut interp = EntityInterpolator::new();

        // No stack reported at all: the neutral default, so a consumer that
        // multiplies by count never draws zero copies of nothing.
        interp.update(&[item_snap(9, Vec3::new(1.0, 64.0, 2.0))], 0.016);
        assert_eq!(interp.draws()[0].count, 1);

        let mut with_count = item_snap_with(9, Vec3::new(1.0, 64.0, 2.0), Some(stone()));
        with_count.count = 64;
        interp.update(std::slice::from_ref(&with_count), 0.016);
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
        interp.update_with_view(
            &[item_snap_moving(9, spawn, Some(vel), false)],
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
            interp.update_with_view(
                &[item_snap_moving(9, spawn, Some(vel), false)],
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
    /// server drops it after `take_item_entity`: `fold_snapshots` despawns its track
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
        interp.update(
            &[
                item_snap_with(ITEM, Vec3::ZERO, Some(stone())),
                snap(COLLECTOR, collector_feet, 0.0),
            ],
            0.0,
        );
        assert!(
            begin_item_pickup(interp.world_mut(), ITEM, COLLECTOR),
            "the item was tracked with a reported stack, so a pickup must start"
        );

        // One tick, with the item gone from the server's report.
        interp.update(&[snap(COLLECTOR, collector_feet, 0.0)], TICK);

        let draws = interp.draws();
        let flying: Vec<&EntityDraw> = draws
            .iter()
            .filter(|d| d.type_path == ITEM_ENTITY_TYPE_PATH)
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
        interp.update(
            &[
                item_snap_with(ITEM, Vec3::ZERO, Some(stone())),
                snap(COLLECTOR, collector_feet, 0.0),
            ],
            0.0,
        );
        interp.update(&[snap(COLLECTOR, collector_feet, 0.0)], TICK);
        assert!(
            !interp
                .draws()
                .iter()
                .any(|d| d.type_path == ITEM_ENTITY_TYPE_PATH),
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
        interp.update(
            &[
                item_snap_with(ITEM, Vec3::ZERO, Some(stone())),
                snap(COLLECTOR, collector_feet, 0.0),
            ],
            0.0,
        );
        assert!(begin_item_pickup(interp.world_mut(), ITEM, COLLECTOR));

        let mut drawn = Vec::new();
        for _ in 0..5 {
            interp.update(&[snap(COLLECTOR, collector_feet, 0.0)], TICK);
            drawn.push(
                interp
                    .draws()
                    .iter()
                    .filter(|d| d.type_path == ITEM_ENTITY_TYPE_PATH)
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
        interp.update(&[item_snap(7, Vec3::ZERO)], 0.0);
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
        interp.update(&[item_snap_with(1, Vec3::ZERO, Some(stone()))], 0.0);
        assert!(begin_item_pickup(interp.world_mut(), 1, 4242));
        interp.update(&[], TICK);
        assert!(
            !interp
                .draws()
                .iter()
                .any(|d| d.type_path == ITEM_ENTITY_TYPE_PATH),
            "an unresolvable collector must draw nothing rather than aim at the origin"
        );
        for _ in 0..3 {
            interp.update(&[], TICK);
        }
        assert!(
            interp.world().resource::<PickupAnimations>().is_empty(),
            "the animation must still expire, or an out-of-range collector leaks one \
             entry per pickup for the whole session"
        );
    }
}

