//! The "what consumes this" gate for plugin-driven entity spawn/despawn and
//! custom entity type registration: a real
//! `bevy_ecs` `App` with `lodestone_ecs::CorePlugin` + [`MobSpawnerPlugin`],
//! driven only through queued requests and a real [`GameTick`] run — never by
//! calling `lodestone_ecs::entity_spawn`'s functions directly, which would be
//! a closed loop rather than proof anything actually consumes the surface.
//!
//! Every assertion reads back through [`EntityIndex`] — the same resource
//! `lodestone_shell::entities::fold_entities` walks every frame to build its
//! render-side track, generically, off whatever component set an entry
//! carries — so this is the same "reaches the real read path" shape
//! `lodestone-worldedit`'s own real-schedule test uses for `ChunkWorld`.

use lodestone_ecs::app::App;
use lodestone_ecs::entity::{CustomName, EntityIndex, EntityKind, Health, Position};
use lodestone_ecs::entity_spawn::CustomEntityKind;
use lodestone_ecs::{CorePlugin, GameTick, LocalPlayer};
use lodestone_mob_spawner::{
    DespawnRequests, MobSpawnerPlugin, SpawnRequest, SpawnRequests, SpawnedEntities, TRAINING_DUMMY,
};
use lodestone_model::{Rotation, Text, Vec3};

fn app() -> App {
    let mut app = App::new();
    app.add_plugins((CorePlugin, MobSpawnerPlugin));
    app
}

fn key(s: &str) -> lodestone_model::ResourceKey {
    s.parse().expect("valid resource key")
}

/// Spawn a plain vanilla entity through a queued request, tick the
/// schedule, modify it through an ordinary component write (the "modify"
/// half — already plugin-writable per `docs/plugin-api.md`), then despawn it
/// through a second queued request — all read back through the real
/// `EntityIndex`.
#[test]
fn a_queued_spawn_reaches_the_index_can_be_modified_and_a_queued_despawn_removes_it() {
    let mut app = app();

    app.world_mut()
        .resource_mut::<SpawnRequests>()
        .0
        .push(SpawnRequest::Vanilla {
            kind: key("minecraft:cow"),
            position: Vec3::new(3.0, 70.0, -1.0),
            rotation: Rotation::new(180.0, 0.0),
        });

    // Nothing applies until the schedule actually runs — proves the request
    // is genuinely drained by a system, not applied eagerly by pushing it.
    assert!(app.world().resource::<EntityIndex>().is_empty());

    app.world_mut().run_schedule(GameTick);

    let ids = app.world().resource::<SpawnedEntities>().0.clone();
    assert_eq!(ids.len(), 1, "exactly one entity must have been spawned");
    let id = ids[0];
    assert!(id < 0, "a plugin-spawned entity must carry a negative id");

    let entity = app
        .world()
        .resource::<EntityIndex>()
        .get(id)
        .expect("the spawned entity must be indexed — this is what a real mesher/fold reads");
    assert_eq!(
        app.world().get::<EntityKind>(entity).map(|k| k.0.clone()),
        Some(key("minecraft:cow"))
    );
    assert_eq!(
        app.world().get::<Position>(entity).map(|p| p.0),
        Some(Vec3::new(3.0, 70.0, -1.0))
    );

    // The "modify" half of the spawn/despawn/modify API: ordinary component mutation, already
    // plugin-writable — proving nothing about the new spawn/despawn API
    // regressed that capability.
    app.world_mut()
        .entity_mut(entity)
        .insert((Position(Vec3::new(10.0, 80.0, 10.0)), Health(15.0)));
    assert_eq!(
        app.world().get::<Position>(entity).map(|p| p.0),
        Some(Vec3::new(10.0, 80.0, 10.0)),
        "a plugin must be able to freely move a spawned entity, same as any tracked entity"
    );
    assert_eq!(app.world().get::<Health>(entity).map(|h| h.0), Some(15.0));

    app.world_mut().resource_mut::<DespawnRequests>().0.push(id);
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world().resource::<EntityIndex>().get(id),
        None,
        "despawn must remove the index entry"
    );
    assert!(
        app.world().get_entity(entity).is_err(),
        "despawn must remove the underlying ECS entity"
    );
    assert!(
        app.world().resource::<SpawnedEntities>().0.is_empty(),
        "the despawned id must drop out of the plugin's own live-entity list"
    );
}

/// The custom entity type [`MobSpawnerPlugin`] registers at build time
/// is spawnable through the exact same queued-request path, and its
/// `EntityKind` on the wire (well — in local rendering, per this crate's
/// module doc on the wire ceiling) is the vanilla disguise, never the
/// logical kind.
#[test]
fn the_registered_training_dummy_type_spawns_disguised_as_a_zombie() {
    let mut app = app();

    app.world_mut()
        .resource_mut::<SpawnRequests>()
        .0
        .push(SpawnRequest::TrainingDummy {
            position: Vec3::new(0.0, 64.0, 0.0),
            rotation: Rotation::new(0.0, 0.0),
        });
    app.world_mut().run_schedule(GameTick);

    let id = app.world().resource::<SpawnedEntities>().0[0];
    let entity = app
        .world()
        .resource::<EntityIndex>()
        .get(id)
        .expect("the training dummy must be indexed");

    assert_eq!(
        app.world().get::<EntityKind>(entity).map(|k| k.0.clone()),
        Some(key("minecraft:zombie")),
        "EntityKind must be the vanilla disguise — a render-side model/texture \
         lookup or the mob census must see an ordinary zombie, never an \
         unmodeled custom key"
    );
    assert_eq!(
        app.world()
            .get::<CustomEntityKind>(entity)
            .map(|k| k.0.clone()),
        Some(key(TRAINING_DUMMY)),
        "the plugin must be able to recover the true logical kind it spawned"
    );

    // The disguise is a real, rigged, textured vanilla mob — free to carry an
    // ordinary custom name, exactly like any other entity a plugin can modify.
    app.world_mut()
        .entity_mut(entity)
        .insert(CustomName(Some(Text::literal("Training Dummy"))));
    assert_eq!(
        app.world().get::<CustomName>(entity).map(|n| n.0.clone()),
        Some(Some(Text::literal("Training Dummy")))
    );
}

/// Negative control: despawning an id nothing tracks (nothing was ever
/// spawned, and the `LocalPlayer`'s own id is never touched) must be a no-op,
/// not a panic and not a spurious removal.
#[test]
fn despawning_an_untracked_id_is_a_harmless_no_op() {
    let mut app = app();
    let local = app.world_mut().spawn(LocalPlayer).id();
    app.world_mut().resource_mut::<EntityIndex>().insert(42, local);

    app.world_mut().resource_mut::<DespawnRequests>().0.push(42);
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world().resource::<EntityIndex>().get(42),
        Some(local),
        "control: the LocalPlayer guard must survive a despawn request routed \
         through the real schedule, not just the bare function"
    );
    assert!(app.world().get_entity(local).is_ok());
}
