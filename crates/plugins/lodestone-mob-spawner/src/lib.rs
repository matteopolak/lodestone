//! A minigame/mob-farm-class plugin: queued entity spawn/despawn requests,
//! plus one custom entity type registered at build time.
//!
//! # What this is
//!
//! The real consumer of [`lodestone_ecs::entity_spawn`] —
//! "everything a Bukkit `World.spawnEntity(loc, type)`/`Entity.remove()` caller
//! would want, minus the wire": a minigame manager spawning a training dummy,
//! a mob-farm plugin spawning stock, a disguise plugin wanting a fake mob with
//! no server involved. Like [`lodestone_worldedit`], this is a *second* plugin
//! conceptually — a real spawner plugin is exactly the kind of thing a
//! third-party author would write on top of the engine, not a part of it.
//!
//! # How it works
//!
//! [`SpawnRequests`]/[`DespawnRequests`] are queued-request resources, the
//! same shape `lodestone_worldedit::FillRequests` uses and for the same
//! reason (`docs/plugin-api.md`'s note on `ActionQueue` winning over a bevy
//! `Message` for a "needs synchronous drain-time application" case):
//! [`apply_spawn_requests`]/[`apply_despawn_requests`] drain them once per
//! [`GameTick`], calling straight through to
//! [`lodestone_ecs::entity_spawn::spawn_entity`]/[`spawn_custom_entity`](lodestone_ecs::entity_spawn::spawn_custom_entity)/[`despawn_entity`](lodestone_ecs::entity_spawn::despawn_entity).
//! [`SpawnedEntities`] is the "return value" a Bukkit `spawnEntity` caller
//! gets back — the ids currently live, so a driver never has to reach into
//! the ECS itself to learn what landed.
//!
//! [`MobSpawnerPlugin::build`] also registers one custom entity type —
//! [`TRAINING_DUMMY`], disguised as `minecraft:zombie` — demonstrating the
//! custom entity type registration path end to end: [`SpawnRequest::TrainingDummy`] resolves
//! through that same registration, not by calling
//! [`lodestone_ecs::entity_spawn::spawn_custom_entity`] with a hand-built
//! registry, which is what makes the crate's own tests a real consumer rather
//! than a closed loop.
//!
//! # How to change it
//!
//! This crate is transport, matching `lodestone_worldedit`'s own "how to
//! change it" note: request validation and the id-safety/namespace rules
//! belong in `lodestone_ecs::entity_spawn`, not here. Adding a rule in this
//! crate instead would mean a second spawner plugin, built directly on the
//! engine surface, obeying different rules than this one.
//!
//! # Dependencies
//!
//! [`lodestone_ecs::entity_spawn`] for the spawn/despawn/registration surface;
//! `lodestone_model` for the value types a request carries.

use bevy_ecs::query::With;
use bevy_ecs::resource::Resource;
use bevy_ecs::system::{Commands, Query, Res, ResMut};
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::entity::EntityIndex;
use lodestone_ecs::entity_spawn::{
    CustomEntityRegistry, CustomEntityTypesExt, EntitySpawn, PluginEntityIds, despawn_entity,
    spawn_custom_entity, spawn_entity,
};
use lodestone_ecs::{GameTick, LocalPlayer};
use lodestone_model::{ResourceKey, Rotation, Vec3};

/// The custom entity kind [`MobSpawnerPlugin`] registers at build time,
/// disguised as `minecraft:zombie`. A logical id in this crate's own
/// namespace, never `minecraft:` — see
/// [`lodestone_ecs::entity_spawn::CustomEntityRegistry::register`]'s doc for
/// why that would be refused.
pub const TRAINING_DUMMY: &str = "lodestone-mob-spawner:training_dummy";
const TRAINING_DUMMY_DISGUISE: &str = "minecraft:zombie";

fn training_dummy_kind() -> ResourceKey {
    TRAINING_DUMMY.parse().expect("valid resource key")
}

/// A queued request to spawn one entity — the shape a real plugin's chat
/// command or minigame-round-start hook would push.
#[derive(Debug, Clone)]
pub enum SpawnRequest {
    /// A plain vanilla entity, exactly as `spawn_entity` builds it.
    Vanilla {
        kind: ResourceKey,
        position: Vec3,
        rotation: Rotation,
    },
    /// [`TRAINING_DUMMY`], resolved through the registry
    /// [`MobSpawnerPlugin::build`] populated.
    TrainingDummy { position: Vec3, rotation: Rotation },
}

/// Requests queued since the last drain.
#[derive(Resource, Debug, Default)]
pub struct SpawnRequests(pub Vec<SpawnRequest>);

/// Entity ids queued for despawn since the last drain.
#[derive(Resource, Debug, Default)]
pub struct DespawnRequests(pub Vec<i32>);

/// Every id this plugin has spawned that has not yet been despawned — the
/// "return value" a Bukkit `spawnEntity` caller gets back, published so a
/// driver can read what actually landed without reaching into the ECS.
#[derive(Resource, Debug, Default)]
pub struct SpawnedEntities(pub Vec<i32>);

/// Drains [`SpawnRequests`], spawning each through
/// [`lodestone_ecs::entity_spawn`] and recording the assigned id in
/// [`SpawnedEntities`].
fn apply_spawn_requests(
    mut requests: ResMut<SpawnRequests>,
    mut commands: Commands,
    mut index: ResMut<EntityIndex>,
    mut ids: ResMut<PluginEntityIds>,
    registry: Res<CustomEntityRegistry>,
    mut spawned: ResMut<SpawnedEntities>,
) {
    for request in requests.0.drain(..) {
        let id = match request {
            SpawnRequest::Vanilla {
                kind,
                position,
                rotation,
            } => {
                spawn_entity(
                    &mut commands,
                    &mut index,
                    &mut ids,
                    EntitySpawn::new(kind, position, rotation),
                )
                .0
            }
            SpawnRequest::TrainingDummy { position, rotation } => spawn_custom_entity(
                &mut commands,
                &mut index,
                &mut ids,
                &registry,
                training_dummy_kind(),
                position,
                rotation,
            )
            .expect("MobSpawnerPlugin registers the training dummy type at build time")
            .0,
        };
        spawned.0.push(id);
    }
}

/// Drains [`DespawnRequests`], removing each id through the same
/// `LocalPlayer`-guarded [`despawn_entity`] a hand-written plugin system would
/// call, and dropping it from [`SpawnedEntities`] once it is actually gone.
fn apply_despawn_requests(
    mut requests: ResMut<DespawnRequests>,
    mut commands: Commands,
    mut index: ResMut<EntityIndex>,
    locals: Query<(), With<LocalPlayer>>,
    mut spawned: ResMut<SpawnedEntities>,
) {
    for entity_id in requests.0.drain(..) {
        if despawn_entity(&mut commands, &mut index, &locals, entity_id) {
            spawned.0.retain(|&id| id != entity_id);
        }
    }
}

/// Installs [`SpawnRequests`]/[`DespawnRequests`]/[`SpawnedEntities`], the
/// systems draining them into [`lodestone_ecs::entity_spawn`], and registers
/// [`TRAINING_DUMMY`].
///
/// Also idempotently installs [`EntityIndex`], matching
/// `lodestone_ecs::ingest::IngestPlugin`'s own note on sharing
/// `ControlledVehicle` with `LocalPlayerPlugin`: a consumer that adds this
/// plugin with no `IngestPlugin` in sight (a headless spawner with no
/// networking at all) still gets a working `EntityIndex`, and adding both in
/// either order leaves a populated one alone.
#[derive(Debug, Default)]
pub struct MobSpawnerPlugin;

impl Plugin for MobSpawnerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EntityIndex>();
        app.init_resource::<SpawnRequests>();
        app.init_resource::<DespawnRequests>();
        app.init_resource::<SpawnedEntities>();
        app.add_custom_entity_type(
            training_dummy_kind(),
            TRAINING_DUMMY_DISGUISE
                .parse()
                .expect("valid resource key"),
        )
        .expect("the training dummy type registers exactly once per App");
        app.add_systems(GameTick, (apply_spawn_requests, apply_despawn_requests));
    }
}
