//! Per-species appearance fields reach the draw's base sheet: `ClientEvent` ->
//! the real `IngestPlugin`/`EntityInterpPlugin` -> the real
//! `extract_entity_draws` -> `EntityDraw::{variant_sheet, overlay_sheet}`.
//!
//! Expected sheets are the pack's own file names. Each family has a control
//! that differs in the one input the claim is about (an unreported field keeps
//! the model's own sheet, a recessive panda gene without its pair shows the
//! normal panda, a wild cat wears no collar).
//!
//! No GPU: the subject is the producer.

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{
    ClientEvent, EntityMetadataUpdate, EntityVariant, MobAppearance, Rotation, Vec3 as ModelVec3,
};

fn appearance(f: impl FnOnce(&mut MobAppearance)) -> EntityMetadataUpdate {
    let mut a = MobAppearance::default();
    f(&mut a);
    EntityMetadataUpdate { appearance: a, ..Default::default() }
}

fn world(cases: &[(i32, &str, EntityMetadataUpdate)]) -> World {
    world_at(0, cases)
}

fn world_at(age: i64, cases: &[(i32, &str, EntityMetadataUpdate)]) -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());
    world.insert_resource(lodestone_ecs::WorldTime { age, time_of_day: 0 });
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

fn keyed(key: &str) -> EntityMetadataUpdate {
    EntityMetadataUpdate {
        variant: Some(EntityVariant::Keyed(key.parse().unwrap())),
        ..Default::default()
    }
}

#[test]
fn registry_keyed_cats_and_frogs_pick_their_own_sheet() {
    let world = world(&[
        (1, "minecraft:cat", keyed("minecraft:siamese")),
        (2, "minecraft:cat", EntityMetadataUpdate::default()),
        (3, "minecraft:frog", keyed("minecraft:cold")),
        (4, "minecraft:cat", keyed("mypack:siamese")),
    ]);
    assert_eq!(draw_for(&world, 1).variant_sheet, Some("entity/cat/cat_siamese"));
    assert_eq!(draw_for(&world, 2).variant_sheet, None, "control: unreported keeps the model sheet");
    assert_eq!(draw_for(&world, 3).variant_sheet, Some("entity/frog/frog_cold"));
    assert_eq!(draw_for(&world, 4).variant_sheet, None, "control: a data-pack key has no vanilla sheet");
}

#[test]
fn a_tamed_cat_wears_a_dyed_collar_and_a_wild_one_does_not() {
    let world = world(&[
        (
            1,
            "minecraft:cat",
            EntityMetadataUpdate { tamed: Some(true), collar_color: Some(11), ..Default::default() },
        ),
        (
            2,
            "minecraft:cat",
            EntityMetadataUpdate { tamed: Some(false), collar_color: Some(11), ..Default::default() },
        ),
    ]);
    let collar = draw_for(&world, 1).overlay_sheet.expect("tamed cat wears a collar");
    assert_eq!(collar.sheet, "entity/cat/cat_collar");
    assert_eq!(collar.tint, [0x3C, 0x44, 0xAA], "dye 11 is blue");
    assert_eq!(draw_for(&world, 2).overlay_sheet, None, "control: not tamed");
}

#[test]
fn rabbit_parrot_llama_and_mooshroom_ordinals_pick_their_sheet() {
    let world = world(&[
        (1, "minecraft:rabbit", appearance(|a| a.rabbit_type = Some(99))),
        (2, "minecraft:rabbit", appearance(|a| a.rabbit_type = Some(1))),
        (3, "minecraft:parrot", appearance(|a| a.parrot_variant = Some(4))),
        (4, "minecraft:llama", appearance(|a| a.llama_variant = Some(2))),
        (5, "minecraft:mooshroom", appearance(|a| a.mooshroom_type = Some(1))),
        (6, "minecraft:rabbit", EntityMetadataUpdate::default()),
    ]);
    assert_eq!(draw_for(&world, 1).variant_sheet, Some("entity/rabbit/rabbit_caerbannog"));
    assert_eq!(draw_for(&world, 2).variant_sheet, Some("entity/rabbit/rabbit_white"));
    assert_eq!(draw_for(&world, 3).variant_sheet, Some("entity/parrot/parrot_grey"));
    assert_eq!(draw_for(&world, 4).variant_sheet, Some("entity/llama/llama_brown"));
    assert_eq!(draw_for(&world, 5).variant_sheet, Some("entity/cow/mooshroom_brown"));
    assert_eq!(draw_for(&world, 6).variant_sheet, None, "control: unreported rabbit is the default brown");
}

#[test]
fn a_recessive_panda_gene_shows_only_with_its_pair() {
    let genes = |main: u8, hidden: u8| {
        appearance(move |a| {
            a.panda_main_gene = Some(main);
            a.panda_hidden_gene = Some(hidden);
        })
    };
    let world = world(&[
        (1, "minecraft:panda", genes(4, 4)),
        (2, "minecraft:panda", genes(4, 1)),
        (3, "minecraft:panda", genes(1, 5)),
        (4, "minecraft:panda", genes(6, 0)),
    ]);
    assert_eq!(draw_for(&world, 1).variant_sheet, Some("entity/panda/panda_brown"));
    assert_eq!(draw_for(&world, 2).variant_sheet, Some("entity/panda/panda"), "control: brown without its pair");
    assert_eq!(draw_for(&world, 3).variant_sheet, Some("entity/panda/panda_lazy"), "a dominant gene always shows");
    assert_eq!(draw_for(&world, 4).variant_sheet, Some("entity/panda/panda_aggressive"));
}

#[test]
fn a_shulkers_dye_and_a_beess_nectar_pick_their_sheet() {
    let world = world(&[
        (1, "minecraft:shulker", appearance(|a| a.shulker_color = Some(11))),
        (2, "minecraft:shulker", appearance(|a| a.shulker_color = Some(16))),
        (3, "minecraft:bee", appearance(|a| a.bee_flags = Some(0x08))),
        (4, "minecraft:bee", appearance(|a| a.bee_flags = Some(0x02))),
    ]);
    assert_eq!(draw_for(&world, 1).variant_sheet, Some("entity/shulker/shulker_blue"));
    assert_eq!(draw_for(&world, 2).variant_sheet, Some("entity/shulker/shulker"));
    assert_eq!(draw_for(&world, 3).variant_sheet, Some("entity/bee/bee_nectar"));
    assert_eq!(draw_for(&world, 4).variant_sheet, None, "control: rolling is not nectar");
}

fn villager(kind: &str, profession: &str, level: i32) -> EntityMetadataUpdate {
    EntityMetadataUpdate {
        variant: Some(EntityVariant::Villager {
            kind: kind.parse().unwrap(),
            profession: profession.parse().unwrap(),
            level,
        }),
        ..Default::default()
    }
}

fn layer_sheets(world: &World, id: i32) -> Vec<&'static str> {
    draw_for(world, id).layers.iter().map(|l| l.sheet).collect()
}

#[test]
fn villagers_draw_type_profession_and_level_layers() {
    let world = world(&[
        (1, "minecraft:villager", villager("minecraft:desert", "minecraft:farmer", 3)),
        (2, "minecraft:villager", villager("minecraft:plains", "minecraft:nitwit", 1)),
        (3, "minecraft:villager", villager("minecraft:plains", "minecraft:none", 1)),
        (4, "minecraft:zombie_villager", villager("minecraft:snow", "minecraft:cleric", 9)),
        (5, "minecraft:villager", villager("minecraft:plains", "mypack:smith", 1)),
        (6, "minecraft:cow", villager("minecraft:plains", "minecraft:farmer", 1)),
    ]);
    assert_eq!(
        layer_sheets(&world, 1),
        [
            "entity/villager/type/desert",
            "entity/villager/profession/farmer",
            "entity/villager/profession_level/gold",
        ]
    );
    assert_eq!(
        layer_sheets(&world, 2),
        ["entity/villager/type/plains", "entity/villager/profession/nitwit"],
        "a nitwit has no level badge"
    );
    assert_eq!(layer_sheets(&world, 3), ["entity/villager/type/plains"], "no profession, type only");
    assert_eq!(
        layer_sheets(&world, 4),
        [
            "entity/zombie_villager/type/snow",
            "entity/zombie_villager/profession/cleric",
            "entity/zombie_villager/profession_level/diamond",
        ],
        "level clamps to the last badge"
    );
    assert_eq!(layer_sheets(&world, 5), ["entity/villager/type/plains"], "a data-pack profession has no vanilla sheet");
    assert!(layer_sheets(&world, 6).is_empty(), "control: a cow has no clothing layers");
}

#[test]
fn a_wolfs_anger_lasts_until_its_end_time_on_the_world_clock() {
    let angry = |end: i64| appearance(move |a| a.wolf_anger_end_time = Some(end));
    let cases = [
        (1, "minecraft:wolf", angry(300)),
        (2, "minecraft:wolf", angry(0)),
        (3, "minecraft:wolf", EntityMetadataUpdate::default()),
    ];
    let during = world_at(100, &cases);
    assert_eq!(draw_for(&during, 1).variant_sheet, Some("entity/wolf/wolf_angry"));
    assert_eq!(draw_for(&during, 2).variant_sheet, None, "control: end time zero is calm");
    assert_eq!(draw_for(&during, 3).variant_sheet, None, "control: never angered");
    let after = world_at(400, &cases);
    assert_eq!(draw_for(&after, 1).variant_sheet, None, "control: the end time has passed");
}

#[test]
fn a_beess_anger_and_nectar_combine_on_the_world_clock() {
    let bee = |flags: Option<u8>, end: Option<i64>| {
        appearance(move |a| {
            a.bee_flags = flags;
            a.bee_anger_end_time = end;
        })
    };
    let cases = [
        (1, "minecraft:bee", bee(None, Some(300))),
        (2, "minecraft:bee", bee(Some(0x08), Some(300))),
        (3, "minecraft:bee", bee(Some(0x08), None)),
    ];
    let during = world_at(100, &cases);
    assert_eq!(draw_for(&during, 1).variant_sheet, Some("entity/bee/bee_angry"));
    assert_eq!(draw_for(&during, 2).variant_sheet, Some("entity/bee/bee_angry_nectar"));
    assert_eq!(draw_for(&during, 3).variant_sheet, Some("entity/bee/bee_nectar"), "control: nectar alone");
    let after = world_at(400, &cases);
    assert_eq!(draw_for(&after, 1).variant_sheet, None, "control: anger expired");
    assert_eq!(draw_for(&after, 2).variant_sheet, Some("entity/bee/bee_nectar"), "expired anger keeps the nectar");
}
