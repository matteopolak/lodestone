//! Glowing-eyes layers reach the draw: `ClientEvent` -> the real
//! `IngestPlugin`/`EntityInterpPlugin` -> the real `extract_entity_draws` ->
//! `EntityDraw::eyes_sheet`.
//!
//! Expected sheets are the pack's own file names. A creaking's eyes depend on
//! its awake flag, so the control is the same species with the flag off or
//! never reported (the wire omits a default-valued field).
//!
//! No GPU: the subject is the producer.

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{
    ClientEvent, EntityMetadataUpdate, MobAppearance, Rotation, Vec3 as ModelVec3,
};

const SPIDER: i32 = 1;
const CAVE_SPIDER: i32 = 2;
const ENDERMAN: i32 = 3;
const PHANTOM: i32 = 4;
const AWAKE_CREAKING: i32 = 5;
const DORMANT_CREAKING: i32 = 6;
const COW: i32 = 7;

fn world() -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());
    let spawns = [
        (SPIDER, "minecraft:spider"),
        (CAVE_SPIDER, "minecraft:cave_spider"),
        (ENDERMAN, "minecraft:enderman"),
        (PHANTOM, "minecraft:phantom"),
        (AWAKE_CREAKING, "minecraft:creaking"),
        (DORMANT_CREAKING, "minecraft:creaking"),
        (COW, "minecraft:cow"),
    ];
    for (id, kind) in spawns {
        world.resource_mut::<IngestQueue>().push(ClientEvent::EntitySpawned {
            entity_id: id,
            uuid: None,
            entity_type: kind.parse().expect("valid entity type key"),
            pos: ModelVec3::new(0.0, 64.0, 0.0),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        });
        world.run_schedule(NetIngest);
    }
    for (id, active) in [(AWAKE_CREAKING, true), (DORMANT_CREAKING, false)] {
        world.resource_mut::<IngestQueue>().push(ClientEvent::EntityMetadataUpdated {
            entity_id: id,
            metadata: EntityMetadataUpdate {
                appearance: MobAppearance { creaking_active: Some(active), ..Default::default() },
                ..Default::default()
            },
        });
    }
    world.run_schedule(NetIngest);
    fold_entities(&mut world);
    world.run_schedule(GameTick);
    world.run_schedule(Extract);
    world
}

fn draw_for(world: &World, id: i32) -> EntityDraw {
    extracted_entity_draws(world)
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("entity {id} not among the extracted draws"))
}

#[test]
fn eyed_species_draw_their_own_eyes_sheet() {
    let world = world();
    assert_eq!(draw_for(&world, SPIDER).eyes_sheet, Some("entity/spider/spider_eyes"));
    assert_eq!(draw_for(&world, CAVE_SPIDER).eyes_sheet, Some("entity/spider/spider_eyes"));
    assert_eq!(draw_for(&world, ENDERMAN).eyes_sheet, Some("entity/enderman/enderman_eyes"));
    assert_eq!(draw_for(&world, PHANTOM).eyes_sheet, Some("entity/phantom/phantom_eyes"));
    assert_eq!(draw_for(&world, COW).eyes_sheet, None, "control: a cow has no eyes layer");
}

#[test]
fn a_creakings_eyes_follow_its_active_flag() {
    let world = world();
    assert_eq!(
        draw_for(&world, AWAKE_CREAKING).eyes_sheet,
        Some("entity/creaking/creaking_eyes")
    );
    assert_eq!(
        draw_for(&world, DORMANT_CREAKING).eyes_sheet,
        None,
        "control: same species, flag off"
    );
}
