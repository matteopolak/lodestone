//! A fox's coat, an axolotl's colour and a wolf's collar reach the draw:
//! `ClientEvent` → the real `IngestPlugin`/`EntityInterpPlugin` → the real
//! `extract_entity_draws` → `EntityDraw::{variant_sheet, overlay_sheet}`.
//!
//! The expected sheets and tints come from the pack's file names and from the
//! dye table's own literal bytes (blue `0x3C44AA`, red `0xB02E26`), not from the
//! functions under test. Each claim has a control whose only difference is the
//! one input the claim is about.
//!
//! No GPU: the subject is the producer.

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{
    ClientEvent, EntityMetadataUpdate, EntityVariant, Rotation, Vec3 as ModelVec3,
};

const SNOW_FOX: i32 = 1;
const RED_FOX: i32 = 2;
const GOLD_AXOLOTL: i32 = 3;
const BLUE_WOLF: i32 = 4;
const WILD_WOLF: i32 = 5;
const DEFAULT_COLLAR_WOLF: i32 = 6;
const COW: i32 = 7;

fn world() -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());

    let spawns = [
        (SNOW_FOX, "minecraft:fox"),
        (RED_FOX, "minecraft:fox"),
        (GOLD_AXOLOTL, "minecraft:axolotl"),
        (BLUE_WOLF, "minecraft:wolf"),
        (WILD_WOLF, "minecraft:wolf"),
        (DEFAULT_COLLAR_WOLF, "minecraft:wolf"),
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

    let updates = [
        (SNOW_FOX, EntityMetadataUpdate {
            variant: Some(EntityVariant::Fox { snow: true }),
            ..Default::default()
        }),
        (RED_FOX, EntityMetadataUpdate {
            variant: Some(EntityVariant::Fox { snow: false }),
            ..Default::default()
        }),
        (GOLD_AXOLOTL, EntityMetadataUpdate {
            variant: Some(EntityVariant::Axolotl { color: 2 }),
            ..Default::default()
        }),
        (BLUE_WOLF, EntityMetadataUpdate {
            tamed: Some(true),
            collar_color: Some(11),
            ..Default::default()
        }),
        // Collar reported, not tamed: vanilla draws no collar on a wild wolf.
        (WILD_WOLF, EntityMetadataUpdate {
            tamed: Some(false),
            collar_color: Some(11),
            ..Default::default()
        }),
        // Tamed with the default collar: the wire carries no collar field.
        (DEFAULT_COLLAR_WOLF, EntityMetadataUpdate {
            tamed: Some(true),
            ..Default::default()
        }),
        // A collar on a cow must not draw anything.
        (COW, EntityMetadataUpdate {
            tamed: Some(true),
            collar_color: Some(11),
            ..Default::default()
        }),
    ];
    for (id, metadata) in updates {
        world.resource_mut::<IngestQueue>().push(ClientEvent::EntityMetadataUpdated {
            entity_id: id,
            metadata,
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
fn a_fox_coat_selects_its_own_sheet() {
    let world = world();
    assert_eq!(draw_for(&world, SNOW_FOX).variant_sheet, Some("entity/fox/fox_snow"));
    assert_eq!(draw_for(&world, RED_FOX).variant_sheet, Some("entity/fox/fox"));
}

#[test]
fn an_axolotl_colour_selects_its_own_sheet() {
    let world = world();
    assert_eq!(
        draw_for(&world, GOLD_AXOLOTL).variant_sheet,
        Some("entity/axolotl/axolotl_gold")
    );
}

#[test]
fn a_tamed_wolf_draws_a_dye_tinted_collar_and_a_wild_one_does_not() {
    let world = world();
    let blue = draw_for(&world, BLUE_WOLF).overlay_sheet.expect("tamed wolf wears a collar");
    assert_eq!(blue.sheet, "entity/wolf/wolf_collar");
    assert_eq!(blue.tint, [0x3C, 0x44, 0xAA], "dye 11 is blue");

    assert_eq!(
        draw_for(&world, WILD_WOLF).overlay_sheet,
        None,
        "control: same collar report, not tamed"
    );

    let default = draw_for(&world, DEFAULT_COLLAR_WOLF)
        .overlay_sheet
        .expect("a tamed wolf with no collar field wears the default");
    assert_eq!(default.tint, [0xB0, 0x2E, 0x26], "the default collar is red");

    assert_eq!(draw_for(&world, COW).overlay_sheet, None, "control: not a wolf");
}
