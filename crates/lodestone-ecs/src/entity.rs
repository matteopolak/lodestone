//! The Stage-1 entity component set: one copy of every entity's
//! server-reported state, held as `bevy_ecs` components rather than as a
//! `HashMap<i32, EntityView>` in `lodestone_client::state::Inner`.
//!
//! # How `Reported<T>`'s three states survive the move to components
//!
//! `lodestone_model::Reported<T>` distinguishes three things, and all three are
//! load-bearing (`docs/bevy-migration.md`, Stage 1's "gotcha that will bite"):
//!
//! | `Reported<T>` | component representation |
//! |---|---|
//! | `Unreported` — the server has never mentioned the field | **component absent** |
//! | `Reported(None)` — the server explicitly cleared it | component **present**, inner `None` |
//! | `Reported(Some(v))` — the server set a value | component present, inner `Some(v)` |
//!
//! That is the plan's prescribed encoding, and it is strictly clearer than the
//! nested `Option` was — but only if nothing ever spawns these components with
//! a default. **A dropped item announces its stack exactly once, at spawn, and
//! then sends item-free metadata for the rest of its life**, so a
//! [`DisplayItem`] that were spawned as `DisplayItem(None)` and re-inserted
//! each metadata packet would blank the drop one tick after it appeared — the
//! "dropped item goes invisible" defect. [`apply_entity_spawn`] therefore
//! spawns **no** [`DisplayItem`] and **no** [`CustomName`], and
//! [`apply_entity_metadata`] only inserts one when the update actually carried
//! the field. The unit tests at the bottom of [`crate::ingest`] pin all three
//! states directly.
//!
//! The same "absent means never reported" rule covers the plain `Option` fields
//! of `EntityView` too — [`EntityFlags`], [`Health`], [`Baby`], [`Pose`],
//! [`Variant`], [`CustomNameVisible`], [`Velocity`], [`EntityUuid`] — which is
//! why they are newtypes over the *inner* value rather than over an `Option`.
//! Only the two genuinely three-state fields ([`CustomName`], [`DisplayItem`])
//! wrap an `Option`.
//!
//! # Per-slot nesting in [`Equipment`]
//!
//! `Equipment` is a `Vec<EntityEquipment>`, not a fixed array of `Option`s, for
//! the same reason `EntityView::equipment` was: a slot **absent** from the list
//! is "the server has never mentioned this slot", while a slot present with
//! `item: None` is an explicit "this slot is empty". Flattening to an array
//! loses that, so do not.

use std::collections::HashMap;

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use lodestone_model::{
    EntityAttributeSnapshot, EntityEquipment, EntityPose, EntityVariant, ItemStack, ResourceKey,
    Vec3,
};
use uuid::Uuid;

/// The server-assigned entity id — the key every `ClientEvent` names an entity
/// by, and the interpolation/draw key downstream.
///
/// Present on every networked entity. [`EntityIndex`] maps this back to a
/// `bevy_ecs` [`Entity`] so an id-addressed event can find its components in
/// O(1) without a full scan.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MinecraftEntityId(pub i32);

/// The entity's UUID, when the spawn carried one.
///
/// **Absent** means the spawn did not carry one — the `Option` in
/// `EntityView::uuid`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityUuid(pub Uuid);

/// The entity type's canonical key (`minecraft:pig`, `minecraft:item`, …).
#[derive(Component, Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityKind(pub ResourceKey);

/// Feet position in world space, as last reported.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Position(pub Vec3);

/// Body yaw/pitch, as last reported.
///
/// A newtype over [`lodestone_model::Rotation`] rather than a re-definition:
/// the model type is the version-free vocabulary every `ClientEvent` speaks,
/// and duplicating its fields here would be a second source of truth for the
/// units.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Rotation(pub lodestone_model::Rotation);

/// Absolute head yaw in degrees.
///
/// Tracked separately from [`Rotation`] and never derived from it: vanilla
/// sends it unconditionally at spawn (`add_entity`) and updates it
/// independently via `rotate_head`, because a walking mob's head tracks its
/// target while its body keeps facing its movement direction.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct HeadYaw(pub f32);

/// Last-reported velocity in blocks per tick.
///
/// **Absent** means the server has never reported one, which is a different
/// state from a reported zero (`Velocity(Vec3::ZERO)`) — a dropped item's whole
/// arc depends on the difference, since gravity alone cannot produce an apex.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Velocity(pub Vec3);

/// Whether the server last reported this entity resting on the ground.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnGround(pub bool);

/// The shared entity flags byte (on-fire / crouching / sprinting / swimming /
/// invisible / glowing / fall-flying).
///
/// **Absent** means no metadata packet has reported it yet.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityFlags(pub u8);

/// The entity's custom name.
///
/// One of the two genuinely three-state fields: **absent** is "never
/// reported", `CustomName(None)` is "explicitly cleared", `CustomName(Some(s))`
/// is the name it holds. See the module docs.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct CustomName(pub Option<String>);

/// Whether the custom name renders above the entity. **Absent** until reported.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomNameVisible(pub bool);

/// The entity's pose. **Absent** until reported.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pose(pub EntityPose);

/// Current health in half-hearts (living entities only). **Absent** until
/// reported.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Health(pub f32);

/// Ticks remaining in the current hurt-flash window — vanilla's `hurtTime`
/// countdown. `LivingEntity.handleDamageEvent` (`LivingEntity.java:2044-2049`,
/// folding [`lodestone_model::ClientEvent::EntityDamaged`]) and
/// `LivingEntity.animateHurt` (`LivingEntity.java:1873-1876`, folding
/// [`lodestone_model::ClientEvent::EntityHurtAnimation`]) both reset the
/// identical pair of fields — `hurtDuration = 10; hurtTime = hurtDuration;` —
/// so one countdown here covers both reports. [`crate::ingest::tick_hurt_time`]
/// ages it toward zero, one per `GameTick`, the same rate
/// `LivingEntity.tick()` decrements the vanilla field.
///
/// **Absent** until the first report, like [`Health`]. Nothing in this crate
/// reads the countdown yet — it exists so a render-side hurt tint has real
/// data to key off, not a guessed decay; wiring that consumer is
/// `lodestone-shell::entities`'s, out of this crate's scope.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HurtTime(pub u32);

/// A **remote** entity's arm-swing progress — vanilla's `LivingEntity`
/// `swingTime`/`swinging`/`attackAnim`/`oAttackAnim`, folded from
/// `ClientboundAnimatePacket`'s `SWING_MAIN_HAND` action (id `0`) by
/// [`crate::ingest::apply_entity_animation`] and advanced once per tick by
/// [`crate::ingest::tick_entity_swing`].
///
/// # Why this duplicates three fields of [`lodestone_entity::pose::EntityPose`]
/// instead of embedding it
///
/// `EntityPose` is the *full* per-entity render pose — walk cycle, head/body
/// orientation and age alongside the swing clock — because that is what the
/// **local player's** third-person body (`Sim::body_pose` in
/// `lodestone-shell::sim`) needs: one pose, one entity, one clock. A tracked
/// network entity already has all of those *except* the swing clock, spread
/// across `lodestone-shell::entities`' `WalkAnim`/`InterpFrom`/`InterpTo` — on
/// a **different** `bevy_ecs::Entity`, since `EntityInterpPlugin` spawns a
/// render-side entity per mob distinct from this crate's ingest entity (see
/// `entities.rs`'s `EntityInterpPlugin` docs). Embedding `EntityPose` here
/// would carry a second, unused walk cycle and a body/head orientation nothing
/// reads; this type carries only the three fields (`swing_time`, `swinging`,
/// `swing_duration`) a remote swing actually needs, with the identical
/// algorithm — see [`Self::start_swing`], [`Self::tick`] and
/// [`Self::attack_anim_lerp`], each cross-referencing the `EntityPose` method
/// it mirrors term-for-term.
///
/// **Absent** until the first `SwingMainHand` report, like [`HurtTime`].
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct AttackSwing {
    swing_time: i32,
    swinging: bool,
    swing_duration: i32,
    /// Current tick's swing progress, `0.0..=1.0` — vanilla's `attackAnim`.
    pub attack_anim: f32,
    /// Previous tick's swing progress, for [`Self::attack_anim_lerp`]'s
    /// forward-wrapped interpolation — vanilla's `oAttackAnim`.
    pub o_attack_anim: f32,
}

impl AttackSwing {
    /// Begins a swing, or extends one already running — `LivingEntity.swing`,
    /// mirrored from [`lodestone_entity::pose::EntityPose::start_swing`].
    /// Swallows a restart before the half-way point, which is what turns a
    /// held mine's every-tick `SwingMainHand` report into one continuous arc
    /// instead of a stutter — see that method's doc for the full reasoning.
    pub fn start_swing(&mut self, duration: i32) {
        if !self.swinging || self.swing_time >= duration / 2 || self.swing_time < 0 {
            self.swing_time = -1;
            self.swinging = true;
            self.swing_duration = duration.max(1);
        }
    }

    /// One tick's advance — the swing half of
    /// [`lodestone_entity::pose::EntityPose::tick`] (`LivingEntity.updateSwingTime`).
    /// A no-op sawtooth hold at `0.0` before the first [`Self::start_swing`]
    /// call, since `swing_duration` defaults to `0` and is clamped to at least
    /// `1` in the division below rather than dividing by zero.
    pub fn tick(&mut self) {
        self.o_attack_anim = self.attack_anim;
        if self.swinging {
            self.swing_time += 1;
            if self.swing_time >= self.swing_duration {
                self.swing_time = 0;
                self.swinging = false;
            }
        } else {
            self.swing_time = 0;
        }
        self.attack_anim = self.swing_time.max(0) as f32 / self.swing_duration.max(1) as f32;
    }

    /// Interpolated swing progress for a partial tick — vanilla's
    /// `LivingEntity.getAttackAnim`, identical to
    /// [`lodestone_entity::pose::EntityPose::attack_anim_lerp`]: a negative
    /// delta is wrapped forward by one whole swing so the arm carries forward
    /// to rest instead of rewinding backward through the arc when a swing ends
    /// or restarts mid-tick. See that method's doc for why a plain lerp is
    /// wrong here.
    #[must_use]
    pub fn attack_anim_lerp(&self, partial_tick: f32) -> f32 {
        let mut diff = self.attack_anim - self.o_attack_anim;
        if diff < 0.0 {
            diff += 1.0;
        }
        self.o_attack_anim + diff * partial_tick
    }
}

/// Whether the entity is a baby (ageable mobs only). **Absent** until reported.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Baby(pub bool);

/// The entity's cosmetic variant (sheep colour, villager profession, …).
///
/// **Absent** means the server sent no variant override, and a consumer should
/// draw the entity type's vanilla default — which is a different state from a
/// known-but-plain variant. Do not read absence as "unknown".
#[derive(Component, Debug, Clone, PartialEq)]
pub struct Variant(pub EntityVariant);

/// The entity's attributes, keyed by canonical id, as `update_attributes` last
/// reported them. Later snapshots for the same attribute replace earlier ones.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct Attributes(pub Vec<EntityAttributeSnapshot>);

/// What the entity is wearing and holding, as `set_equipment` last reported it.
///
/// A slot absent from the list is "never mentioned"; a slot present with
/// `item: None` is an explicit clear. See the module docs on why this is a list
/// of pairs rather than an array.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct Equipment(pub Vec<EntityEquipment>);

/// The item stack this entity *displays* — a dropped item's entire visible
/// identity, plus the display item of thrown projectiles and the eye of ender.
///
/// The second of the two three-state fields: **absent** is "never reported",
/// `DisplayItem(None)` is the server's explicit empty stack (which vanilla
/// draws as nothing), `DisplayItem(Some(stack))` is the stack it holds. See the
/// module docs — this is the component the "dropped item goes invisible"
/// regression lives in.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct DisplayItem(pub Option<ItemStack>);

/// Server entity id → `bevy_ecs` [`Entity`].
///
/// Maintained eagerly by [`apply_entity_spawn`](crate::ingest::apply_entity_spawn)
/// and [`apply_entity_removal`](crate::ingest::apply_entity_removal) rather than
/// rebuilt by a scan, so a movement event in the *same* ingest batch as the
/// spawn it follows can still find its entity.
///
/// This is azalea's `EntityIdIndex` (`azalea-client/src/client.rs`) in
/// miniature, minus the per-client partition — we are a single client, so one
/// global index is the whole story.
#[derive(Resource, Debug, Default)]
pub struct EntityIndex(HashMap<i32, Entity>);

impl EntityIndex {
    /// The ECS entity for a server entity id, if it is currently tracked.
    #[must_use]
    pub fn get(&self, entity_id: i32) -> Option<Entity> {
        self.0.get(&entity_id).copied()
    }

    /// Records `entity` as the holder of `entity_id`, replacing any previous
    /// mapping (servers reuse ids freely).
    pub fn insert(&mut self, entity_id: i32, entity: Entity) {
        self.0.insert(entity_id, entity);
    }

    /// Forgets `entity_id`, returning the ECS entity it mapped to.
    pub fn remove(&mut self, entity_id: i32) -> Option<Entity> {
        self.0.remove(&entity_id)
    }

    /// Every tracked `(server id, ECS entity)` pair. Order is unspecified.
    pub fn iter(&self) -> impl Iterator<Item = (i32, Entity)> + '_ {
        self.0.iter().map(|(id, entity)| (*id, *entity))
    }

    /// How many entities are tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no entities are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
