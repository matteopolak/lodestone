//! Plugin-driven entity spawn/despawn/modify, and custom entity type
//! registration.
//!
//! # What this is
//!
//! The two capabilities `docs/plugin-api.md`'s own analysis names as the
//! achievable half of a Bukkit-class `World.spawnEntity`/`Entity.remove()` API
//! and custom entity-type registration, given today's architecture: a
//! plugin creating and destroying a **local, non-networked** entity, and
//! registering a logical entity kind of its own that disguises as a real
//! vanilla one for rendering. Real Bukkit/Paper solves the same wire ceiling
//! the same way — a custom entity is a vanilla one plus a tag, never a new
//! registry id — and [`lodestone_game::custom_item`] already made that call
//! for items; [`CustomEntityRegistry`] is the entity-shaped mirror of it.
//!
//! "Modify" is not a new capability here: every component in [`crate::entity`]
//! (`Position`, `Rotation`, `Health`, `Equipment`, …) is already plugin-writable
//! per `docs/plugin-api.md`'s "components a plugin can read and write today"
//! table. Ordinary `Commands`/`Query` mutation is the whole API for that half;
//! this module only supplies the two operations a plugin cannot safely
//! reconstruct itself — minting an id that cannot collide with one the server
//! assigns, and despawning through the same [`crate::entity::LocalPlayer`]
//! guard [`crate::ingest::apply_entity_removal`] uses.
//!
//! # Why this reaches pixels with no change to `lodestone-shell`
//!
//! [`crate::entity::EntityIndex`] is exactly what
//! `lodestone_shell::entities::fold_entities` walks every frame to build its
//! render-side track — by id, generically, over whatever
//! [`resolve_entity_facts`](https://docs.rs/lodestone-shell) can read off the
//! entity's components (`EntityKind`, `Position`, `Rotation`, `HeadYaw` are the
//! four it requires; everything else is read optionally). It does not care
//! whether the entry arrived via [`crate::ingest::apply_entity_spawn`] or via
//! [`spawn_entity`] below — both put exactly the same component set on an
//! entity indexed the same way. So a plugin-spawned entity is drawn the next
//! `Extract` with no render-side change at all, which is the property that
//! keeps this from being an island — a subsystem that is built and tested but
//! reaches no pixels because nothing calls it.
//!
//! # Id safety
//!
//! Vanilla's own entity-id counter (`Entity.ENTITY_COUNTER`) starts at `0` and
//! only ever increments, so every id a real server assigns — including the
//! local player's own, via `apply_local_player_login` — is non-negative.
//! [`PluginEntityIds`] mints strictly negative ids, so a plugin-spawned entity
//! can never collide with a server-assigned one: the two ranges do not
//! overlap, by construction, rather than by a runtime check that could miss a
//! case. This is the same hazard `docs/entity-components.md` names for a
//! *reused* wire id landing on `LocalPlayer` — [`despawn_entity`] closes it the
//! same way [`crate::ingest::apply_entity_removal`] already does, by refusing
//! to touch an id currently held by a `LocalPlayer` entity.
//!
//! # Usage
//!
//! ```
//! use lodestone_ecs::app::{App, Plugin};
//! use lodestone_ecs::entity::EntityIndex;
//! use lodestone_ecs::entity_spawn::{
//!     CustomEntityRegistry, EntitySpawn, EntitySpawnPlugin, PluginEntityIds, despawn_entity,
//!     spawn_entity,
//! };
//! use lodestone_ecs::CorePlugin;
//! use bevy_ecs::system::RunSystemOnce;
//!
//! let mut app = App::new();
//! app.add_plugins((CorePlugin, EntitySpawnPlugin));
//!
//! let (id, _entity) = app.world_mut().run_system_once(
//!     |mut commands: bevy_ecs::system::Commands,
//!      mut index: bevy_ecs::system::ResMut<EntityIndex>,
//!      mut ids: bevy_ecs::system::ResMut<PluginEntityIds>| {
//!         spawn_entity(
//!             &mut commands,
//!             &mut index,
//!             &mut ids,
//!             EntitySpawn::new(
//!                 "minecraft:cow".parse().unwrap(),
//!                 lodestone_model::Vec3::new(0.0, 64.0, 0.0),
//!                 lodestone_model::Rotation::new(0.0, 0.0),
//!             ),
//!         )
//!     },
//! ).unwrap();
//! assert!(id < 0, "a plugin-minted id is always negative");
//! ```
//!
//! # How to change it, and the gotchas
//!
//! * **[`PluginEntityIds`] is installed by [`crate::CorePlugin`] itself**, not
//!   only by [`EntitySpawnPlugin`] — [`spawn_entity`] is basic enough, and a
//!   missing resource behind a `ResMut<T>` system parameter panics at runtime
//!   with no compile-time warning, that every `App` in the tree should have
//!   it, the same reasoning that put `WorldTime`/`FrameClock` there. Do not
//!   also fold [`CustomEntityRegistry`] in there: it follows
//!   [`crate::items::CustomItemsPlugin`]'s precedent of an opt-in resource that
//!   self-installs the moment a plugin actually registers something, via
//!   [`CustomEntityTypesExt::add_custom_entity_type`].
//! * **A custom kind's `EntityKind` component always holds the disguise, never
//!   the logical kind.** This is what keeps a plugin-registered type out of
//!   the render-side model corpus's "no model, no texture" assertions —
//!   `lodestone-render`'s `model_for_type`-style lookup only ever sees a real
//!   vanilla key. [`CustomEntityKind`] carries the true logical kind
//!   alongside it, for a plugin that wants to recover what it actually spawned.
//! * **Do not add a fallback that resolves an unregistered custom kind to some
//!   default disguise.** [`spawn_custom_entity`] refuses with
//!   [`UnknownCustomEntityType`] instead — silently drawing a plugin's zombie
//!   disguise as, say, a pig because nobody registered it yet is a worse
//!   failure than a returned error.
//!
//! # Dependencies
//!
//! [`crate::entity`] for the component set and [`crate::entity::EntityIndex`];
//! `lodestone_model` for [`lodestone_model::ResourceKey`]/`Vec3`/`Rotation`;
//! `bevy_ecs`/`bevy_app`. No protocol crate — a plugin-spawned entity never
//! names a numeric id.

use std::collections::BTreeMap;

use bevy_app::{App, Plugin};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::query::With;
use bevy_ecs::resource::Resource;
use bevy_ecs::system::{Commands, Query};
use lodestone_model::{ResourceKey, Rotation as ReportedRotation, Vec3};

use crate::entity::{
    Attributes, EntityIndex, EntityKind, Equipment, HeadYaw, MinecraftEntityId, OnGround, Position,
    Rotation,
};
use crate::player::LocalPlayer;

/// The namespace every vanilla entity kind (and every disguise a custom kind
/// resolves to) is registered under. Mirrors
/// `lodestone_game::custom_item::VANILLA_ITEM_NAMESPACE` exactly, for exactly
/// the same reason: a disguise outside this namespace cannot be encoded on the
/// wire (and, for today's client-only rendering, is not a key any render-side
/// lookup will ever resolve).
pub const VANILLA_ENTITY_NAMESPACE: &str = "minecraft";

/// Mints entity ids for plugin-spawned entities that never came off the wire.
///
/// See the module doc's "Id safety" section for why strictly-negative ids
/// make a collision with a server-assigned id structurally impossible rather
/// than merely unlikely.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginEntityIds {
    next: i32,
}

impl Default for PluginEntityIds {
    fn default() -> Self {
        // The first minted id is `-1`; `0` is left unclaimed by either range so
        // "negative" and "non-negative" stay the whole, simple test — see
        // `is_plugin_entity_id`.
        Self { next: -1 }
    }
}

impl PluginEntityIds {
    /// Mints the next id, monotonically decreasing.
    ///
    /// Saturates at `i32::MIN` rather than wrapping into the non-negative
    /// range — a single session spawning more than two billion plugin
    /// entities is not a real scenario, but wrapping into the server's own
    /// range would be, if it ever happened.
    pub fn reserve(&mut self) -> i32 {
        let id = self.next;
        self.next = self.next.saturating_sub(1);
        id
    }
}

/// Whether `entity_id` is provably a plugin-minted id rather than one a real
/// server could have assigned.
#[must_use]
pub fn is_plugin_entity_id(entity_id: i32) -> bool {
    entity_id < 0
}

/// Everything needed to spawn a fresh, non-networked entity.
#[derive(Debug, Clone)]
pub struct EntitySpawn {
    /// The entity kind rendering keys off — a real vanilla key for a plain
    /// spawn, or a disguise resolved from [`CustomEntityRegistry`] for
    /// [`spawn_custom_entity`].
    pub kind: ResourceKey,
    pub position: Vec3,
    pub rotation: ReportedRotation,
}

impl EntitySpawn {
    #[must_use]
    pub fn new(kind: ResourceKey, position: Vec3, rotation: ReportedRotation) -> Self {
        Self {
            kind,
            position,
            rotation,
        }
    }
}

/// Spawns `spawn` as a fresh ECS entity, indexed under a freshly-minted
/// [`PluginEntityIds`] id, with exactly the component set
/// [`crate::ingest::apply_entity_spawn`] gives a wire-spawned entity of the
/// same kind (minus the fields a spawn packet may or may not carry — `uuid`,
/// `velocity` — which a plugin has no wire report for and so never had cause
/// to set). This is what makes the result indistinguishable, to
/// `lodestone_shell::entities::fold_entities`, from a mob the server actually
/// sent: see the module doc's "Why this reaches pixels" section.
///
/// Returns the minted id and the underlying `bevy_ecs::Entity`.
pub fn spawn_entity(
    commands: &mut Commands,
    index: &mut EntityIndex,
    ids: &mut PluginEntityIds,
    spawn: EntitySpawn,
) -> (i32, Entity) {
    let entity_id = ids.reserve();
    let entity = commands
        .spawn((
            MinecraftEntityId(entity_id),
            EntityKind(spawn.kind),
            Position(spawn.position),
            Rotation(spawn.rotation),
            HeadYaw(spawn.rotation.yaw),
            OnGround(true),
            Attributes::default(),
            Equipment::default(),
        ))
        .id();
    index.insert(entity_id, entity);
    (entity_id, entity)
}

/// Despawns the entity behind `entity_id`.
///
/// Refuses to touch an id currently held by a [`LocalPlayer`] entity — the
/// identical guard [`crate::ingest::apply_entity_removal`] applies to a
/// wire-reported removal, needed here for the same reason: nothing stops a
/// caller naming an id the local player happens to hold (today only possible
/// if a future id-reuse path collides, since [`PluginEntityIds`]' own ids
/// never do).
///
/// Returns `false` if `entity_id` was not tracked, or was refused by the
/// `LocalPlayer` guard; `true` if an entity was despawned.
pub fn despawn_entity(
    commands: &mut Commands,
    index: &mut EntityIndex,
    locals: &Query<(), With<LocalPlayer>>,
    entity_id: i32,
) -> bool {
    if index
        .get(entity_id)
        .is_some_and(|held| locals.contains(held))
    {
        return false;
    }
    let Some(entity) = index.remove(entity_id) else {
        return false;
    };
    commands.entity(entity).despawn();
    true
}

/// The plugin-defined logical entity kind an entity actually is.
///
/// Present only on an entity spawned through [`spawn_custom_entity`]. The
/// entity's [`EntityKind`] component still carries the *disguise* — the real
/// vanilla key rendering resolves — so a lookup keyed on `EntityKind` alone
/// (a render-side model/texture table, a mob-census predicate) sees a
/// perfectly ordinary vanilla entity and never has to know a plugin was
/// involved. A plugin that wants to recover what it actually spawned reads
/// this component instead.
#[derive(Component, Debug, Clone, PartialEq, Eq, Hash)]
pub struct CustomEntityKind(pub ResourceKey);

/// A plugin's registered mapping from a custom logical entity kind to the
/// vanilla [`ResourceKey`] it disguises as.
///
/// The entity-shaped mirror of `lodestone_game::custom_item::CustomItemRegistry`
/// — same wire ceiling (a genuinely novel registry id is not representable),
/// same fix (a vanilla base plus a tag the plugin recognises), same two
/// namespace rules.
#[derive(Resource, Debug, Default)]
pub struct CustomEntityRegistry {
    disguises: BTreeMap<ResourceKey, ResourceKey>,
}

impl CustomEntityRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `custom` as disguised as `disguise`.
    ///
    /// # Errors
    ///
    /// [`CustomEntityTypeError::ReservedNamespace`] if `custom` is
    /// `minecraft:`-namespaced (it would collide with the real registry the
    /// moment anything tried to resolve it); [`CustomEntityTypeError::NonVanillaDisguise`]
    /// if `disguise` is not (a non-vanilla disguise cannot be rendered, for the
    /// same reason a non-vanilla `base` cannot be encoded for a custom item);
    /// [`CustomEntityTypeError::Duplicate`] if `custom` is already registered —
    /// refused rather than replaced, so two plugins claiming one id surfaces at
    /// the registrant instead of silently reassigning the first plugin's type.
    pub fn register(
        &mut self,
        custom: ResourceKey,
        disguise: ResourceKey,
    ) -> Result<(), CustomEntityTypeError> {
        if custom.namespace() == VANILLA_ENTITY_NAMESPACE {
            return Err(CustomEntityTypeError::ReservedNamespace(custom));
        }
        if disguise.namespace() != VANILLA_ENTITY_NAMESPACE {
            return Err(CustomEntityTypeError::NonVanillaDisguise { custom, disguise });
        }
        if self.disguises.contains_key(&custom) {
            return Err(CustomEntityTypeError::Duplicate(custom));
        }
        self.disguises.insert(custom, disguise);
        Ok(())
    }

    /// Removes a registration, returning the disguise it held.
    pub fn unregister(&mut self, custom: &ResourceKey) -> Option<ResourceKey> {
        self.disguises.remove(custom)
    }

    /// The vanilla disguise registered for `custom`, if any.
    #[must_use]
    pub fn disguise(&self, custom: &ResourceKey) -> Option<&ResourceKey> {
        self.disguises.get(custom)
    }

    /// How many custom kinds are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.disguises.len()
    }

    /// Whether no custom kinds are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.disguises.is_empty()
    }
}

/// Why [`CustomEntityRegistry::register`] refused a definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomEntityTypeError {
    /// The custom kind's own id is in the `minecraft:` namespace.
    ReservedNamespace(ResourceKey),
    /// The disguise is not a `minecraft:` entity kind, so it cannot be
    /// rendered (or, on a real server, encoded on the wire).
    NonVanillaDisguise {
        /// The custom kind being defined.
        custom: ResourceKey,
        /// The offending disguise.
        disguise: ResourceKey,
    },
    /// Another definition already claims this custom kind.
    Duplicate(ResourceKey),
}

impl std::fmt::Display for CustomEntityTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReservedNamespace(custom) => write!(
                f,
                "custom entity type `{custom}` is in the reserved `{VANILLA_ENTITY_NAMESPACE}:` \
                 namespace"
            ),
            Self::NonVanillaDisguise { custom, disguise } => write!(
                f,
                "custom entity type `{custom}` disguises as `{disguise}`, which is not a \
                 `{VANILLA_ENTITY_NAMESPACE}:` entity kind and so cannot be rendered"
            ),
            Self::Duplicate(custom) => {
                write!(f, "custom entity type `{custom}` is already registered")
            }
        }
    }
}

impl std::error::Error for CustomEntityTypeError {}

/// [`spawn_custom_entity`] was asked for a custom kind nothing registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCustomEntityType(pub ResourceKey);

impl std::fmt::Display for UnknownCustomEntityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "custom entity type `{}` is not registered", self.0)
    }
}

impl std::error::Error for UnknownCustomEntityType {}

/// Spawns a plugin-registered custom entity kind, resolving it through
/// `registry` to its vanilla disguise.
///
/// The spawned entity's [`EntityKind`] carries the disguise; [`CustomEntityKind`]
/// carries `custom_kind` itself. See the module doc for why a fallback disguise
/// is deliberately not offered for an unregistered kind.
///
/// # Errors
///
/// [`UnknownCustomEntityType`] if `custom_kind` was never registered.
pub fn spawn_custom_entity(
    commands: &mut Commands,
    index: &mut EntityIndex,
    ids: &mut PluginEntityIds,
    registry: &CustomEntityRegistry,
    custom_kind: ResourceKey,
    position: Vec3,
    rotation: ReportedRotation,
) -> Result<(i32, Entity), UnknownCustomEntityType> {
    let Some(disguise) = registry.disguise(&custom_kind).cloned() else {
        return Err(UnknownCustomEntityType(custom_kind));
    };
    let entity_id = ids.reserve();
    let entity = commands
        .spawn((
            MinecraftEntityId(entity_id),
            EntityKind(disguise),
            CustomEntityKind(custom_kind),
            Position(position),
            Rotation(rotation),
            HeadYaw(rotation.yaw),
            OnGround(true),
            Attributes::default(),
            Equipment::default(),
        ))
        .id();
    index.insert(entity_id, entity);
    Ok((entity_id, entity))
}

/// Installs [`EntityIndex`] and [`CustomEntityRegistry`] for a plugin that
/// wants entity spawning with no networking `App` around it.
///
/// [`PluginEntityIds`] is *not* installed here — [`crate::CorePlugin`] already
/// does, since every `App` in the tree installs that one; see this module's
/// "How to change it" section for why. Every `init_resource` call here is
/// idempotent, so adding this plugin alongside [`crate::ingest::IngestPlugin`]
/// (which also installs [`EntityIndex`]) in either order leaves a populated
/// index alone — the same reasoning `IngestPlugin` already documents for
/// sharing `ControlledVehicle` with `LocalPlayerPlugin`.
#[derive(Debug, Default)]
pub struct EntitySpawnPlugin;

impl Plugin for EntitySpawnPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EntityIndex>();
        app.init_resource::<CustomEntityRegistry>();
    }
}

/// `App`-level custom entity type registration, so a plugin's `build` reads as
/// one call — mirrors [`crate::items::CustomItemsExt`] exactly.
pub trait CustomEntityTypesExt {
    /// Registers a custom entity type, installing [`CustomEntityRegistry`]
    /// first if absent.
    ///
    /// # Errors
    ///
    /// [`CustomEntityTypeError`], as [`CustomEntityRegistry::register`].
    fn add_custom_entity_type(
        &mut self,
        custom: ResourceKey,
        disguise: ResourceKey,
    ) -> Result<&mut Self, CustomEntityTypeError>;
}

impl CustomEntityTypesExt for App {
    fn add_custom_entity_type(
        &mut self,
        custom: ResourceKey,
        disguise: ResourceKey,
    ) -> Result<&mut Self, CustomEntityTypeError> {
        self.init_resource::<CustomEntityRegistry>();
        self.world_mut()
            .resource_mut::<CustomEntityRegistry>()
            .register(custom, disguise)?;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::system::RunSystemOnce;
    use bevy_ecs::world::World;

    use super::*;

    fn key(s: &str) -> ResourceKey {
        s.parse().expect("valid resource key")
    }

    fn rotation() -> ReportedRotation {
        ReportedRotation::new(45.0, 0.0)
    }

    fn position() -> Vec3 {
        Vec3::new(1.0, 64.0, 2.0)
    }

    /// A bare `World` with just this module's resources — no `IngestPlugin`,
    /// no networking — matching the "a plugin with no networking `App`
    /// around it" case [`EntitySpawnPlugin`]'s doc names.
    fn bare_world() -> World {
        let mut app = App::new();
        app.add_plugins(EntitySpawnPlugin);
        app.init_resource::<PluginEntityIds>();
        std::mem::take(app.world_mut())
    }

    /// The primary spawn/despawn assertion: spawning, then despawning, an
    /// entity that never came off the wire, through the real `EntityIndex` a
    /// render-side fold would walk.
    #[test]
    fn a_spawned_entity_is_indexed_and_despawn_removes_it() {
        let mut world = bare_world();
        let (id, entity) = world.run_system_once(
            |mut commands: Commands, mut index: bevy_ecs::system::ResMut<EntityIndex>, mut ids: bevy_ecs::system::ResMut<PluginEntityIds>| {
                spawn_entity(
                    &mut commands,
                    &mut index,
                    &mut ids,
                    EntitySpawn::new(key("minecraft:cow"), position(), rotation()),
                )
            },
        ).expect("system runs");

        assert!(is_plugin_entity_id(id), "a plugin-minted id must be negative");
        assert_eq!(world.resource::<EntityIndex>().get(id), Some(entity));
        assert_eq!(
            world.get::<EntityKind>(entity).map(|k| k.0.clone()),
            Some(key("minecraft:cow"))
        );
        assert_eq!(world.get::<Position>(entity).map(|p| p.0), Some(position()));

        let despawned = world
            .run_system_once(
                move |mut commands: Commands,
                 mut index: bevy_ecs::system::ResMut<EntityIndex>,
                 locals: Query<(), With<LocalPlayer>>| {
                    despawn_entity(&mut commands, &mut index, &locals, id)
                },
            )
            .expect("system runs");
        assert!(despawned, "a tracked plugin entity must be despawned");
        assert_eq!(world.resource::<EntityIndex>().get(id), None);
        assert!(world.get_entity(entity).is_err(), "the ECS entity must be gone");
    }

    /// Negative control mirroring `apply_entity_removal`'s own guard: an id
    /// held by a `LocalPlayer` must survive a despawn call naming it.
    #[test]
    fn despawn_entity_refuses_an_id_held_by_the_local_player() {
        let mut world = bare_world();
        let local = world.spawn(LocalPlayer).id();
        world.resource_mut::<EntityIndex>().insert(7, local);

        let despawned = world
            .run_system_once(
                |mut commands: Commands,
                 mut index: bevy_ecs::system::ResMut<EntityIndex>,
                 locals: Query<(), With<LocalPlayer>>| {
                    despawn_entity(&mut commands, &mut index, &locals, 7)
                },
            )
            .expect("system runs");
        assert!(!despawned, "control failed: the LocalPlayer guard did not fire");
        assert_eq!(world.resource::<EntityIndex>().get(7), Some(local));
        assert!(world.get_entity(local).is_ok(), "the local player must survive");
    }

    #[test]
    fn plugin_ids_are_strictly_negative_and_decreasing() {
        let mut ids = PluginEntityIds::default();
        let first = ids.reserve();
        let second = ids.reserve();
        assert!(first < 0 && second < 0);
        assert!(second < first, "ids must not repeat");
    }

    #[test]
    fn custom_entity_type_resolves_to_its_disguise() {
        let mut registry = CustomEntityRegistry::new();
        registry
            .register(key("myrpg:training_dummy"), key("minecraft:zombie"))
            .expect("valid registration");
        assert_eq!(
            registry.disguise(&key("myrpg:training_dummy")),
            Some(&key("minecraft:zombie"))
        );
    }

    #[test]
    fn a_reserved_namespace_custom_id_is_refused() {
        let mut registry = CustomEntityRegistry::new();
        let err = registry
            .register(key("minecraft:training_dummy"), key("minecraft:zombie"))
            .unwrap_err();
        assert!(matches!(err, CustomEntityTypeError::ReservedNamespace(_)));
    }

    #[test]
    fn a_non_vanilla_disguise_is_refused() {
        let mut registry = CustomEntityRegistry::new();
        let err = registry
            .register(key("myrpg:training_dummy"), key("myrpg:not_vanilla"))
            .unwrap_err();
        assert!(matches!(err, CustomEntityTypeError::NonVanillaDisguise { .. }));
    }

    #[test]
    fn a_duplicate_custom_id_is_refused() {
        let mut registry = CustomEntityRegistry::new();
        registry
            .register(key("myrpg:training_dummy"), key("minecraft:zombie"))
            .expect("first registration succeeds");
        let err = registry
            .register(key("myrpg:training_dummy"), key("minecraft:pig"))
            .unwrap_err();
        assert!(matches!(err, CustomEntityTypeError::Duplicate(_)));
    }

    /// Custom entity type registration's whole point: the spawned entity's `EntityKind` is the disguise
    /// (what a model/texture lookup and the mob census see), never the
    /// logical kind — [`CustomEntityKind`] is where the plugin recovers that.
    #[test]
    fn spawn_custom_entity_carries_the_disguise_in_entity_kind() {
        let mut world = bare_world();
        world
            .resource_mut::<CustomEntityRegistry>()
            .register(key("myrpg:training_dummy"), key("minecraft:zombie"))
            .expect("valid registration");

        let (id, entity) = world
            .run_system_once(
                |mut commands: Commands,
                 mut index: bevy_ecs::system::ResMut<EntityIndex>,
                 mut ids: bevy_ecs::system::ResMut<PluginEntityIds>,
                 registry: bevy_ecs::system::Res<CustomEntityRegistry>| {
                    spawn_custom_entity(
                        &mut commands,
                        &mut index,
                        &mut ids,
                        &registry,
                        key("myrpg:training_dummy"),
                        position(),
                        rotation(),
                    )
                    .expect("registered kind")
                },
            )
            .expect("system runs");

        assert!(is_plugin_entity_id(id));
        assert_eq!(
            world.get::<EntityKind>(entity).map(|k| k.0.clone()),
            Some(key("minecraft:zombie")),
            "EntityKind must carry the disguise, never the custom kind"
        );
        assert_eq!(
            world.get::<CustomEntityKind>(entity).map(|k| k.0.clone()),
            Some(key("myrpg:training_dummy")),
            "CustomEntityKind must carry the true logical kind"
        );
    }

    /// Control: an unregistered custom kind must be refused outright, never
    /// silently rendered as some default disguise.
    #[test]
    fn spawn_custom_entity_refuses_an_unregistered_kind() {
        let mut world = bare_world();
        let result = world
            .run_system_once(
                |mut commands: Commands,
                 mut index: bevy_ecs::system::ResMut<EntityIndex>,
                 mut ids: bevy_ecs::system::ResMut<PluginEntityIds>,
                 registry: bevy_ecs::system::Res<CustomEntityRegistry>| {
                    spawn_custom_entity(
                        &mut commands,
                        &mut index,
                        &mut ids,
                        &registry,
                        key("myrpg:never_registered"),
                        position(),
                        rotation(),
                    )
                },
            )
            .expect("system runs");
        assert_eq!(
            result,
            Err(UnknownCustomEntityType(key("myrpg:never_registered")))
        );
    }
}
