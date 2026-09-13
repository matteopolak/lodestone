//! Production metadata-to-draw witness for player wearable state.
//!
//! The fixtures enter through `IngestQueue`, cross the same interpolation and
//! extraction schedules used by the running client, and are inspected only at
//! `EntityDraw::anim`. This keeps the test focused on the hand-off that can
//! otherwise leave a decoded field stranded before a render consumer sees it.
//! The unreported player is the negative control: absent optional metadata
//! retains the renderer's ordinary cape, rest-wing, and zero-motion defaults.

use bevy_ecs::world::World;
use lodestone::entities::{extracted_entity_draws, fold_entities, EntityDraw, EntityInterpPlugin};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, NetIngest};
use lodestone_model::{
    ClientEvent, EntityMetadataUpdate, EntityPose, Rotation, Vec3 as ModelVec3,
};

const GLIDING_PLAYER: i32 = 1;
const UNREPORTED_PLAYER: i32 = 2;
const CROUCHING_PLAYER: i32 = 3;

fn world_with_player_render_state() -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());

    for (id, velocity) in [
        (GLIDING_PLAYER, Some(ModelVec3::new(0.4, -0.2, 0.3))),
        (UNREPORTED_PLAYER, None),
        (CROUCHING_PLAYER, Some(ModelVec3::new(0.1, 0.0, -0.1))),
    ] {
        world
            .resource_mut::<IngestQueue>()
            .push(ClientEvent::EntitySpawned {
                entity_id: id,
                uuid: None,
                entity_type: "minecraft:player".parse().expect("valid entity type key"),
                pos: ModelVec3::new(0.0, 64.0, 0.0),
                rotation: Rotation::new(0.0, 0.0),
                velocity,
            });
        world.run_schedule(NetIngest);
    }

    world
        .resource_mut::<IngestQueue>()
        .push(ClientEvent::EntityMetadataUpdated {
            entity_id: GLIDING_PLAYER,
            metadata: EntityMetadataUpdate {
                flags: Some(0x80),
                player_model_customization: Some(0x00),
                pose: Some(EntityPose::FallFlying),
                ..EntityMetadataUpdate::default()
            },
        });
    world
        .resource_mut::<IngestQueue>()
        .push(ClientEvent::EntityMetadataUpdated {
            entity_id: CROUCHING_PLAYER,
            metadata: EntityMetadataUpdate {
                player_model_customization: Some(0x01),
                pose: Some(EntityPose::Crouching),
                ..EntityMetadataUpdate::default()
            },
        });
    world.run_schedule(NetIngest);

    fold_entities(&mut world);
    world.run_schedule(Extract);
    world
}

fn draw_for(world: &World, id: i32) -> EntityDraw {
    extracted_entity_draws(world)
        .into_iter()
        .find(|draw| draw.id == id)
        .unwrap_or_else(|| panic!("entity {id} not among extracted draws"))
}

#[test]
fn metadata_state_reaches_the_production_player_draw() {
    let world = world_with_player_render_state();
    let gliding = draw_for(&world, GLIDING_PLAYER);
    let unreported = draw_for(&world, UNREPORTED_PLAYER);
    let crouching = draw_for(&world, CROUCHING_PLAYER);

    assert!(!gliding.anim.cape_visible);
    assert!(gliding.anim.fall_flying);
    assert_eq!(gliding.anim.motion, glam::Vec3::new(0.4, -0.2, 0.3));

    assert!(crouching.anim.cape_visible);
    assert!(crouching.anim.crouching);
    assert!(!crouching.anim.fall_flying);

    assert!(unreported.anim.cape_visible);
    assert!(!unreported.anim.fall_flying);
    assert_eq!(unreported.anim.motion, glam::Vec3::ZERO);
}
