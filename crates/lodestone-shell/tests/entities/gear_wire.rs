//! A saddle or body-slot item on the wire reaches the draw as worn-gear layers:
//! `ClientEvent::EntityEquipmentUpdated` -> the real ingest -> `extract_entity_draws`
//! -> `EntityDraw::gear`.
//!
//! Controls differ in the one input the claim is about: the same animal without the
//! item, a non-saddle item in the saddle slot, a baby wearing the same saddle.

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, GearOverlay, extracted_entity_draws, fold_entities};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{
    ClientEvent, EntityEquipment, EntityMetadataUpdate, EquipmentSlot, ItemStack, Rotation,
    Vec3 as ModelVec3,
};

struct Case {
    id: i32,
    kind: &'static str,
    baby: bool,
    worn: Vec<(EquipmentSlot, &'static str, Option<u32>)>,
}

fn case(id: i32, kind: &'static str, worn: Vec<(EquipmentSlot, &'static str, Option<u32>)>) -> Case {
    Case { id, kind, baby: false, worn }
}

fn world(cases: &[Case]) -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());
    world.insert_resource(lodestone_ecs::WorldTime { age: 0, time_of_day: 0 });
    for c in cases {
        let queue = |world: &mut World, event| world.resource_mut::<IngestQueue>().push(event);
        queue(&mut world, ClientEvent::EntitySpawned {
            entity_id: c.id,
            uuid: None,
            entity_type: c.kind.parse().expect("valid entity type key"),
            pos: ModelVec3::new(0.0, 64.0, 0.0),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        });
        world.run_schedule(NetIngest);
        if c.baby {
            queue(&mut world, ClientEvent::EntityMetadataUpdated {
                entity_id: c.id,
                metadata: EntityMetadataUpdate { baby: Some(true), ..Default::default() },
            });
        }
        let equipment = c
            .worn
            .iter()
            .map(|(slot, item, dye)| {
                let mut stack = ItemStack::new(format!("minecraft:{item}").parse().unwrap(), 1);
                stack.components.dyed_color = *dye;
                EntityEquipment { slot: *slot, item: Some(stack) }
            })
            .collect();
        queue(&mut world, ClientEvent::EntityEquipmentUpdated { entity_id: c.id, equipment });
        world.run_schedule(NetIngest);
    }
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

fn names(gear: &[GearOverlay]) -> Vec<(&'static str, &'static str)> {
    gear.iter().map(|g| (g.model, g.sheet)).collect()
}

#[test]
fn a_saddled_pig_draws_the_saddle_rig_and_only_a_saddle_does() {
    let mut baby = case(4, "minecraft:pig", vec![(EquipmentSlot::Saddle, "saddle", None)]);
    baby.baby = true;
    let world = world(&[
        case(1, "minecraft:pig", vec![(EquipmentSlot::Saddle, "saddle", None)]),
        case(2, "minecraft:pig", vec![]),
        case(3, "minecraft:pig", vec![(EquipmentSlot::Saddle, "carrot_on_a_stick", None)]),
        baby,
    ]);
    assert_eq!(
        names(&draw_for(&world, 1).gear),
        [("pig_saddle", "entity/equipment/pig_saddle/saddle")]
    );
    assert!(draw_for(&world, 2).gear.is_empty(), "control: no item, no layer");
    assert!(draw_for(&world, 3).gear.is_empty(), "control: another item is not a saddle");
    assert!(draw_for(&world, 4).gear.is_empty(), "control: no gear rig fits a baby");
}

#[test]
fn horse_gear_stacks_armour_and_saddle_and_dye_tints_leather() {
    let world = world(&[
        case(1, "minecraft:horse", vec![
            (EquipmentSlot::Body, "diamond_horse_armor", None),
            (EquipmentSlot::Saddle, "saddle", None),
        ]),
        case(2, "minecraft:horse", vec![(EquipmentSlot::Body, "leather_horse_armor", Some(0x00FF_8000))]),
        case(3, "minecraft:horse", vec![(EquipmentSlot::Body, "leather_horse_armor", None)]),
        case(4, "minecraft:skeleton_horse", vec![(EquipmentSlot::Saddle, "saddle", None)]),
        case(5, "minecraft:donkey", vec![(EquipmentSlot::Saddle, "saddle", None)]),
    ]);
    assert_eq!(
        names(&draw_for(&world, 1).gear),
        [
            ("horse_armor", "entity/equipment/horse_body/diamond"),
            ("horse_saddle", "entity/equipment/horse_saddle/saddle"),
        ]
    );
    let dyed = draw_for(&world, 2).gear;
    assert_eq!(dyed.len(), 2, "dyeable base plus the untinted overlay");
    assert_eq!(dyed[0].tint, [0xFF, 0x80, 0x00]);
    assert_eq!(dyed[1].tint, [255; 3]);
    assert_eq!(draw_for(&world, 3).gear[0].tint, [0xA0, 0x65, 0x40], "control: undyed leather");
    assert_eq!(
        names(&draw_for(&world, 4).gear),
        [("undead_horse_saddle", "entity/equipment/skeleton_horse_saddle/saddle")]
    );
    assert_eq!(draw_for(&world, 5).gear[0].model, "donkey_saddle");
}

#[test]
fn llama_carpet_wolf_armour_strider_and_camel_saddles_resolve() {
    let world = world(&[
        case(1, "minecraft:llama", vec![(EquipmentSlot::Body, "red_carpet", None)]),
        case(2, "minecraft:wolf", vec![(EquipmentSlot::Body, "wolf_armor", None)]),
        case(3, "minecraft:wolf", vec![(EquipmentSlot::Body, "wolf_armor", Some(0x0033_66CC))]),
        case(4, "minecraft:strider", vec![(EquipmentSlot::Saddle, "saddle", None)]),
        case(5, "minecraft:camel", vec![(EquipmentSlot::Saddle, "saddle", None)]),
        case(6, "minecraft:llama", vec![(EquipmentSlot::Body, "stone", None)]),
    ]);
    assert_eq!(
        names(&draw_for(&world, 1).gear),
        [("llama_decor", "entity/equipment/llama_body/red")]
    );
    assert_eq!(draw_for(&world, 2).gear.len(), 1, "undyed wolf armour has no dye overlay");
    let dyed = draw_for(&world, 3).gear;
    assert_eq!(dyed.len(), 2);
    assert_eq!(dyed[1].tint, [0x33, 0x66, 0xCC]);
    assert_eq!(draw_for(&world, 4).gear[0].model, "strider");
    assert_eq!(draw_for(&world, 5).gear[0].model, "camel_saddle");
    assert!(draw_for(&world, 6).gear.is_empty(), "control: a non-carpet body item draws nothing");
}
