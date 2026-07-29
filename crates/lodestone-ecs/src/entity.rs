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
