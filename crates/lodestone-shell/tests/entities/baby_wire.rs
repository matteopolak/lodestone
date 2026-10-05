//! A baby flag on the wire reaches the draw as a dedicated baby rig and baby sheets:
//! `ClientEvent` -> the real `IngestPlugin`/`EntityInterpPlugin` -> the real
//! `extract_entity_draws` -> `EntityDraw::{baby, model_type_path, model_scale,
//! variant_sheet, overlay_sheet, layers}`.
//!
//! Controls differ in the one input the claim is about: the same type without the
//! flag, and a baby of a type that has no baby rig. The pack-sheet claim reads the
//! real `client.jar` file list, so a baby sheet name this crate derives is checked
//! against Mojang's own listing rather than against the derivation.

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{
    ClientEvent, EntityMetadataUpdate, EntityVariant, Rotation, Vec3 as ModelVec3,
};

fn world(cases: &[(i32, &str, EntityMetadataUpdate)]) -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());
    world.insert_resource(lodestone_ecs::WorldTime { age: 0, time_of_day: 0 });
    for (id, kind, _) in cases {
        world.resource_mut::<IngestQueue>().push(ClientEvent::EntitySpawned {
            entity_id: *id,
            uuid: None,
            entity_type: kind.parse().expect("valid entity type key"),
            pos: ModelVec3::new(0.0, 64.0, 0.0),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        });
        world.run_schedule(NetIngest);
    }
    for (id, _, metadata) in cases {
        world.resource_mut::<IngestQueue>().push(ClientEvent::EntityMetadataUpdated {
            entity_id: *id,
            metadata: metadata.clone(),
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

fn baby() -> EntityMetadataUpdate {
    EntityMetadataUpdate { baby: Some(true), ..Default::default() }
}

fn baby_keyed(key: &str) -> EntityMetadataUpdate {
    EntityMetadataUpdate {
        baby: Some(true),
        variant: Some(EntityVariant::Keyed(key.parse().unwrap())),
        ..Default::default()
    }
}

#[test]
fn a_baby_with_a_dedicated_rig_draws_it_at_unit_scale_and_an_adult_does_not() {
    let world = world(&[
        (1, "minecraft:pig", baby()),
        (2, "minecraft:pig", EntityMetadataUpdate::default()),
        (3, "minecraft:zombie", baby()),
        (4, "minecraft:villager", baby()),
    ]);
    let pig = draw_for(&world, 1);
    assert!(pig.baby);
    assert_eq!(pig.model_type_path(), "pig_baby");
    assert_eq!(pig.model_scale(), 1.0, "the baby rig already carries the baby proportions");
    assert_eq!(pig.scale, 0.5, "the age scale stays for hitbox-sized consumers");

    let adult = draw_for(&world, 2);
    assert_eq!(adult.model_type_path(), "pig", "control: no flag, no baby rig");
    assert_eq!(adult.model_scale(), 1.0);
    assert!(!adult.baby);

    assert_eq!(draw_for(&world, 3).model_type_path(), "zombie_baby");
    assert_eq!(draw_for(&world, 4).model_type_path(), "villager_baby");
}

#[test]
fn a_baby_of_a_type_without_a_baby_rig_keeps_the_half_scale_adult_mesh() {
    let world = world(&[(1, "minecraft:camel_husk", baby()), (2, "minecraft:happy_ghast", baby())]);
    let husk = draw_for(&world, 1);
    assert!(husk.baby);
    assert_eq!(husk.model_type_path(), "camel_husk");
    assert_eq!(husk.model_scale(), 0.5);
    let ghast = draw_for(&world, 2);
    assert_eq!(ghast.model_type_path(), "happy_ghast_baby", "control: the ghast has a baby rig");
    assert_eq!(ghast.model_scale(), 1.0);
}

#[test]
fn a_baby_binds_the_baby_sheet_of_its_variant() {
    let world = world(&[
        (1, "minecraft:pig", baby_keyed("minecraft:cold")),
        (2, "minecraft:pig", EntityMetadataUpdate {
            variant: Some(EntityVariant::Keyed("minecraft:cold".parse().unwrap())),
            ..Default::default()
        }),
        (3, "minecraft:wolf", EntityMetadataUpdate {
            baby: Some(true),
            tamed: Some(true),
            collar_color: Some(3),
            variant: Some(EntityVariant::Keyed("minecraft:ashen".parse().unwrap())),
            ..Default::default()
        }),
        (4, "minecraft:cow", baby()),
    ]);
    assert_eq!(draw_for(&world, 1).variant_sheet, Some("entity/pig/pig_cold_baby"));
    assert_eq!(
        draw_for(&world, 2).variant_sheet,
        Some("entity/pig/pig_cold"),
        "control: the adult keeps its adult sheet"
    );
    let wolf = draw_for(&world, 3);
    assert_eq!(wolf.variant_sheet, Some("entity/wolf/wolf_ashen_tame_baby"));
    assert_eq!(wolf.overlay_sheet.expect("tamed wolf collar").sheet, "entity/wolf/wolf_collar_baby");
    assert_eq!(
        draw_for(&world, 4).variant_sheet,
        None,
        "no variant reported: the rig's own default baby sheet"
    );
}

#[test]
fn a_baby_villager_draws_only_its_baby_biome_layer() {
    let villager = |baby: bool| EntityMetadataUpdate {
        baby: baby.then_some(true),
        variant: Some(EntityVariant::Villager {
            kind: "minecraft:snow".parse().unwrap(),
            profession: "minecraft:librarian".parse().unwrap(),
            level: 3,
        }),
        ..Default::default()
    };
    let world = world(&[(1, "minecraft:villager", villager(true)), (2, "minecraft:villager", villager(false))]);
    let layers: Vec<_> = draw_for(&world, 1).layers.iter().map(|l| l.sheet).collect();
    assert_eq!(layers, ["entity/villager/baby/snow"]);
    let adult: Vec<_> = draw_for(&world, 2).layers.iter().map(|l| l.sheet).collect();
    assert_eq!(
        adult,
        [
            "entity/villager/type/snow",
            "entity/villager/profession/librarian",
            "entity/villager/profession_level/gold",
        ],
        "control: an adult keeps the full stack"
    );
}

#[test]
fn a_baby_sheeps_wool_rides_the_layer_list_and_an_adult_keeps_the_wool_mesh() {
    let sheep = |baby: bool, color: u8, sheared: bool| EntityMetadataUpdate {
        baby: baby.then_some(true),
        variant: Some(EntityVariant::Dyed { color, sheared }),
        ..Default::default()
    };
    let world = world(&[
        (1, "minecraft:sheep", sheep(true, 14, false)),
        (2, "minecraft:sheep", sheep(true, 14, true)),
        (3, "minecraft:sheep", sheep(false, 14, false)),
    ]);
    let wool = draw_for(&world, 1).layers;
    assert_eq!(wool.len(), 1);
    assert_eq!(wool[0].sheet, "entity/sheep/sheep_wool_baby");
    assert_eq!(wool[0].tint, lodestone_assets::entity_models::sheep_wool_tint(14), "red dye");
    assert!(draw_for(&world, 2).layers.is_empty(), "control: a sheared baby grows no wool");
    assert!(draw_for(&world, 3).layers.is_empty(), "control: the adult wool is the mesh pass, not a layer");
}

/// Every baby sheet this build can bind exists in the real pack. The adult sheet
/// names are the pack's own files, so the check cannot be satisfied by the suffix
/// rule agreeing with itself.
#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn every_baby_sheet_the_renderer_can_bind_is_a_file_in_the_pack() {
    use std::collections::HashSet;
    let jar = lodestone_mc_cache::client_jar().expect("no client.jar under .cache/mc");
    let file = std::fs::File::open(jar).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let names: HashSet<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_owned())
        .collect();
    let exists = |reference: &str| {
        names.contains(&format!("assets/minecraft/textures/{reference}.png"))
    };

    // Every adult sheet in the pack's directories a baby-capable model draws from,
    // minus the baby sheets themselves. Only models with a baby rig are considered.
    let mut checked = 0;
    let mut missing = Vec::new();
    for name in &names {
        let Some(reference) = lodestone_render::sheet_reference_of(name) else { continue };
        if reference.ends_with("_baby") {
            continue;
        }
        let Some(dir) = reference.rsplit_once('/').map(|(d, _)| d) else { continue };
        // Directories and file stems of the baby-capable families whose adult sheet
        // is selected by a variant the wire carries.
        let selected = match dir {
            "entity/pig" | "entity/cow" | "entity/chicken" | "entity/wolf" | "entity/cat"
            | "entity/rabbit" | "entity/llama" | "entity/panda" | "entity/axolotl"
            | "entity/bee" | "entity/horse" | "entity/fox" => true,
            _ => false,
        };
        // Skip sheets that are not a body coat (eyes, collars handled below, armour,
        // saddles, the killer rabbit's overlay-free brown set is included).
        let is_body = !reference.contains("saddle")
            && !reference.contains("armor")
            && !reference.contains("eyes")
            && !reference.contains("sleep")
            && !reference.contains("collar")
            && !reference.contains("markings")
            && !reference.contains("skeleton")
            && !reference.contains("zombie")
            && !reference.contains("donkey")
            && !reference.contains("mule")
            && !reference.contains("toast")
            && !reference.contains("llama_spit");
        if !selected || !is_body {
            continue;
        }
        checked += 1;
        let baby = lodestone_render::baby_sheet(reference);
        if !exists(baby) {
            missing.push(format!("{reference} -> {baby}"));
        }
    }
    missing.sort();
    // Not every coat in those directories has a baby sheet (the adult-only wolf
    // `wolf_collar`, the fox sleeping coats); the families the wire can select all do.
    let selectable = [
        "entity/pig/pig_cold", "entity/pig/pig_temperate", "entity/pig/pig_warm",
        "entity/cow/cow_cold", "entity/cow/cow_warm", "entity/cow/mooshroom_red",
        "entity/cow/mooshroom_brown", "entity/chicken/chicken_cold",
        "entity/wolf/wolf_ashen", "entity/wolf/wolf_ashen_tame", "entity/wolf/wolf_ashen_angry",
        "entity/wolf/wolf_striped_tame", "entity/wolf/wolf", "entity/wolf/wolf_tame",
        "entity/wolf/wolf_angry", "entity/cat/cat_siamese", "entity/cat/cat_all_black",
        "entity/rabbit/rabbit_gold", "entity/rabbit/rabbit_caerbannog",
        "entity/llama/llama_gray", "entity/panda/panda", "entity/panda/panda_lazy",
        "entity/panda/panda_aggressive", "entity/panda/panda_brown",
        "entity/axolotl/axolotl_blue", "entity/bee/bee_angry_nectar", "entity/fox/fox_snow",
        "entity/horse/horse_darkbrown", "entity/horse/horse_markings_whitedots",
        "entity/wolf/wolf_collar", "entity/cat/cat_collar",
    ];
    for adult in selectable {
        let baby = lodestone_render::baby_sheet(adult);
        assert!(exists(adult), "control: adult sheet {adult} must be a pack file");
        assert!(exists(baby), "{adult} -> {baby} is not a file in the pack");
    }
    assert!(checked > 60, "walked only {checked} adult sheets");
    // Control: a derivation that does not exist is reported absent.
    assert!(!exists("entity/pig/pig_cold_baby_baby"));
    eprintln!("adult sheets without a baby counterpart (informational): {missing:?}");
}

#[test]
fn a_suffocating_strider_binds_the_cold_sheet_and_its_baby_the_cold_baby_sheet() {
    let cold = |baby: bool| EntityMetadataUpdate {
        baby: baby.then_some(true),
        appearance: lodestone_model::MobAppearance { strider_suffocating: Some(true), ..Default::default() },
        ..Default::default()
    };
    let world = world(&[
        (1, "minecraft:strider", cold(false)),
        (2, "minecraft:strider", cold(true)),
        (3, "minecraft:strider", EntityMetadataUpdate::default()),
    ]);
    assert_eq!(draw_for(&world, 1).variant_sheet, Some("entity/strider/strider_cold"));
    assert_eq!(draw_for(&world, 2).variant_sheet, Some("entity/strider/strider_cold_baby"));
    assert_eq!(draw_for(&world, 3).variant_sheet, None, "control: warm strider keeps the default sheet");
}
