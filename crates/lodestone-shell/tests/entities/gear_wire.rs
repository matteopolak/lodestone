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
    chested: bool,
    rider: Option<i32>,
    /// `(damage, max_damage)` applied to every worn stack.
    wear: Option<(u32, u32)>,
    id: i32,
    kind: &'static str,
    baby: bool,
    worn: Vec<(EquipmentSlot, &'static str, Option<u32>)>,
}

fn case(id: i32, kind: &'static str, worn: Vec<(EquipmentSlot, &'static str, Option<u32>)>) -> Case {
    Case { chested: false, rider: None, wear: None, id, kind, baby: false, worn }
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
        if c.chested {
            queue(&mut world, ClientEvent::EntityMetadataUpdated {
                entity_id: c.id,
                metadata: EntityMetadataUpdate {
                    appearance: lodestone_model::MobAppearance { chested: Some(true), ..Default::default() },
                    ..Default::default()
                },
            });
        }
        let equipment = c
            .worn
            .iter()
            .map(|(slot, item, dye)| {
                let mut stack = ItemStack::new(format!("minecraft:{item}").parse().unwrap(), 1);
                stack.components.dyed_color = *dye;
                if let Some((damage, max)) = c.wear {
                    stack.components.damage = Some(damage);
                    stack.components.max_damage = Some(max);
                }
                EntityEquipment { slot: *slot, item: Some(stack) }
            })
            .collect();
        queue(&mut world, ClientEvent::EntityEquipmentUpdated { entity_id: c.id, equipment });
        world.run_schedule(NetIngest);
    }
    for c in cases {
        if let Some(rider) = c.rider {
            world.resource_mut::<IngestQueue>().push(ClientEvent::EntityPassengersChanged {
                vehicle_id: c.id,
                passenger_ids: vec![rider],
            });
            world.run_schedule(NetIngest);
        }
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

#[test]
fn the_chest_flag_and_a_passenger_reach_the_draw() {
    let mut chested = case(1, "minecraft:llama", vec![]);
    chested.chested = true;
    let mut ridden = case(3, "minecraft:horse", vec![(EquipmentSlot::Saddle, "saddle", None)]);
    ridden.rider = Some(4);
    let world = world(&[
        chested,
        case(2, "minecraft:llama", vec![]),
        ridden,
        case(4, "minecraft:zombie", vec![]),
        case(5, "minecraft:horse", vec![(EquipmentSlot::Saddle, "saddle", None)]),
    ]);
    assert!(draw_for(&world, 1).chested);
    assert!(!draw_for(&world, 2).chested, "control: no chest flag reported");
    assert!(draw_for(&world, 3).ridden);
    assert!(!draw_for(&world, 5).ridden, "control: a saddled horse with nobody on it");
    assert!(!draw_for(&world, 1).ridden);
}

#[test]
fn damaged_wolf_armour_adds_the_crack_overlay_for_its_wear_level() {
    let worn = |id, wear| {
        let mut c = case(id, "minecraft:wolf", vec![(EquipmentSlot::Body, "wolf_armor", None)]);
        c.wear = wear;
        c
    };
    // 64 durability: 0 damage is full, 40 damage leaves 0.375, 60 leaves 0.0625, 10 leaves 0.84.
    let world = world(&[worn(1, None), worn(2, Some((0, 64))), worn(3, Some((10, 64))), worn(4, Some((40, 64))), worn(5, Some((60, 64)))]);
    let last = |id| draw_for(&world, id).gear.last().map(|g| g.sheet).unwrap();
    assert_eq!(draw_for(&world, 1).gear.len(), 1, "control: no damage component, no cracks");
    assert_eq!(draw_for(&world, 2).gear.len(), 1, "control: full durability, no cracks");
    assert_eq!(last(3), "entity/wolf/wolf_armor_crackiness_low");
    assert_eq!(last(4), "entity/wolf/wolf_armor_crackiness_medium");
    assert_eq!(last(5), "entity/wolf/wolf_armor_crackiness_high");
}

#[test]
fn a_harnessed_happy_ghast_draws_harness_goggles_by_rider_and_ropes_by_leash() {
    let mut baby = case(5, "minecraft:happy_ghast", vec![(EquipmentSlot::Body, "red_harness", None)]);
    baby.baby = true;
    let mut ridden = case(2, "minecraft:happy_ghast", vec![(EquipmentSlot::Body, "red_harness", None)]);
    ridden.rider = Some(9);
    let cases = [
        case(1, "minecraft:happy_ghast", vec![(EquipmentSlot::Body, "blue_harness", None)]),
        ridden,
        case(3, "minecraft:happy_ghast", vec![]),
        case(4, "minecraft:happy_ghast", vec![(EquipmentSlot::Body, "red_harness", None)]),
        baby,
        case(9, "minecraft:zombie", vec![]),
        case(10, "minecraft:cow", vec![]),
    ];
    let mut world = world(&cases);
    // A cow leashed to ghast 4: the holder draws ropes, the others do not.
    world.resource_mut::<IngestQueue>().push(ClientEvent::EntityLeashed { entity_id: 10, holder_id: Some(4) });
    world.run_schedule(NetIngest);
    fold_entities(&mut world);
    world.run_schedule(GameTick);
    world.run_schedule(Extract);
    assert_eq!(
        names(&draw_for(&world, 1).gear),
        [("happy_ghast_harness_idle", "entity/equipment/happy_ghast_body/blue_harness")]
    );
    assert_eq!(draw_for(&world, 2).gear[0].model, "happy_ghast_harness", "goggles down while ridden");
    assert!(draw_for(&world, 3).gear.is_empty(), "control: no harness, no layers");
    let holder = draw_for(&world, 4).gear;
    assert_eq!(holder.len(), 2);
    assert_eq!(holder[1].model, "happy_ghast_ropes");
    assert_eq!(draw_for(&world, 5).gear[0].model, "happy_ghast_baby_harness_idle", "babies keep a harness");
}

#[test]
fn a_drowned_always_wears_its_outer_layer_and_a_zombie_does_not() {
    let mut baby = case(2, "minecraft:drowned", vec![]);
    baby.baby = true;
    let world = world(&[case(1, "minecraft:drowned", vec![]), baby, case(3, "minecraft:zombie", vec![])]);
    assert_eq!(
        names(&draw_for(&world, 1).gear),
        [("drowned_outer", "entity/zombie/drowned_outer_layer")]
    );
    assert_eq!(
        names(&draw_for(&world, 2).gear),
        [("drowned_baby_outer", "entity/zombie/drowned_outer_layer_baby")]
    );
    assert!(draw_for(&world, 3).gear.is_empty(), "control: a zombie has no outer layer");
}

#[test]
fn nautilus_gear_and_the_trader_llama_blanket_reach_the_draw() {
    let mut baby = case(5, "minecraft:trader_llama", vec![]);
    baby.baby = true;
    let world = world(&[
        case(1, "minecraft:nautilus", vec![(EquipmentSlot::Saddle, "saddle", None), (EquipmentSlot::Body, "diamond_nautilus_armor", None)]),
        case(2, "minecraft:zombie_nautilus", vec![(EquipmentSlot::Body, "copper_nautilus_armor", None)]),
        case(3, "minecraft:trader_llama", vec![]),
        case(4, "minecraft:trader_llama", vec![(EquipmentSlot::Body, "red_carpet", None)]),
        baby,
        case(6, "minecraft:llama", vec![]),
    ]);
    assert_eq!(
        names(&draw_for(&world, 1).gear),
        [
            ("nautilus_armor", "entity/equipment/nautilus_body/diamond"),
            ("nautilus_saddle", "entity/equipment/nautilus_saddle/saddle"),
        ]
    );
    assert_eq!(draw_for(&world, 2).gear[0].sheet, "entity/equipment/nautilus_body/copper");
    assert_eq!(names(&draw_for(&world, 3).gear), [("llama_decor", "entity/equipment/llama_body/trader_llama")]);
    assert_eq!(names(&draw_for(&world, 4).gear), [("llama_decor", "entity/equipment/llama_body/red")], "a carpet replaces the blanket");
    assert_eq!(names(&draw_for(&world, 5).gear), [("llama_baby_decor", "entity/equipment/llama_body/trader_llama_baby")]);
    assert!(draw_for(&world, 6).gear.is_empty(), "control: a plain llama wears nothing");
}

/// A baby zombie's helmet on the wire is drawn with the baby armour mesh from the
/// `humanoid_baby` sheet: the draw's rig selects [`ArmourModelSet::baby`], and the
/// resolved layer is the path the client jar lists for the baby diamond sheet.
/// Controls: the same helmet on an adult zombie keeps the adult mesh, and a baby
/// piglin wears the piglin cut.
#[test]
fn a_baby_zombies_helmet_reaches_the_baby_armour_mesh_and_sheet() {
    use lodestone_assets::equipment::{ArmourLayerType, ArmourSlot, BabyArmourKind, armour_texture_path};
    use lodestone_render::entity::{ArmourModelSet, armour_layers_of};

    let mut baby = case(1, "minecraft:zombie", vec![(EquipmentSlot::Head, "diamond_helmet", None)]);
    baby.baby = true;
    let mut piglin = case(3, "minecraft:piglin", vec![(EquipmentSlot::Head, "golden_helmet", None)]);
    piglin.baby = true;
    let world = world(&[baby, case(2, "minecraft:zombie", vec![(EquipmentSlot::Head, "diamond_helmet", None)]), piglin]);
    let armour = ArmourModelSet::load();

    let baby = draw_for(&world, 1);
    assert_eq!(baby.model_type_path(), "zombie_baby");
    assert!(baby.equipment.iter().any(|(slot, id)| *slot == EquipmentSlot::Head && id.path() == "diamond_helmet"));
    let (rig, mesh) = armour.baby(baby.model_type_path(), ArmourSlot::Head).expect("a baby zombie wears the baby helmet");
    assert_eq!(rig, "zombie_baby");
    assert_eq!(mesh.mesh.parts.iter().map(|(name, _)| *name).collect::<Vec<_>>(), ["head"]);
    let layers = armour_layers_of(ArmourSlot::Head, "diamond_helmet", ArmourLayerType::HumanoidBaby);
    assert_eq!(layers.len(), 1);
    // The jar keeps the baby sheets under their own directory (its file list).
    assert_eq!(
        armour_texture_path(&layers[0], ArmourLayerType::HumanoidBaby),
        "assets/minecraft/textures/entity/equipment/humanoid_baby/diamond.png"
    );

    let adult = draw_for(&world, 2);
    assert_eq!(adult.model_type_path(), "zombie");
    assert!(armour.baby(adult.model_type_path(), ArmourSlot::Head).is_none(), "control: an adult keeps the adult mesh");

    let piglin = draw_for(&world, 3);
    assert_eq!(piglin.model_type_path(), "piglin_baby");
    assert!(armour.baby(piglin.model_type_path(), ArmourSlot::Head).is_some());
    assert_eq!(BabyArmourKind::for_baby_rig(piglin.model_type_path()), Some(BabyArmourKind::Piglin));
}
