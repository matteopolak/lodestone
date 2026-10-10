//! Expected values come from the reference rules: the box is 1 x 0.25, the click
//! height is the placement height, the yaw is the nearest quarter turn, and a
//! support check fires on the 101st tick.

use lodestone_data::block_states::{StateId, air_state};
use lodestone_model::{BlockFace, BlockPos, ItemComponents, ItemStack, ResourceKey, Vec3, Vec3f};

use uuid::Uuid;

use super::*;
use crate::mobs::{ChunkWorld, MobHandle};

fn state(name: &str) -> StateId {
    StateId::from_state_str(name).expect("fixture state is registered")
}

/// Stone up to and including `y = 64`, air above, with `extra` overriding single cells.
fn world(extra: Vec<((i32, i32, i32), StateId)>) -> impl Fn(i32, i32, i32) -> StateId {
    let stone = state("minecraft:stone");
    move |x, y, z| {
        if let Some((_, s)) = extra.iter().find(|(at, _)| *at == (x, y, z)) {
            return *s;
        }
        if y <= 64 { stone } else { air_state() }
    }
}

fn top_of(x: i32, y: i32, z: i32) -> (BlockPos, BlockFace, Vec3f) {
    (BlockPos::new(x, y, z), BlockFace::Up, Vec3f { x: 0.5, y: 1.0, z: 0.5 })
}

fn place(color: u8, click: (BlockPos, BlockFace, Vec3f), yaw: f32, w: &dyn Fn(i32, i32, i32) -> StateId) -> CushionUse {
    use_cushion_item(color, click.0, click.1, click.2, None, yaw, w)
}

fn stack(item: &str) -> ItemStack {
    ItemStack::new(item.parse::<ResourceKey>().unwrap(), 1)
}

#[test]
fn a_stone_top_places_at_the_click_height_in_the_cell_above() {
    let w = world(vec![]);
    // Stone fills y 64, so its top face is at 65.0 and the cushion stands there.
    assert_eq!(
        place(14, top_of(3, 64, -2), 100.0, &w),
        CushionUse::Place { position: Vec3::new(3.5, 65.0, -1.5), yaw: 90.0, color: 14 }
    );
}

#[test]
fn a_bottom_slab_top_places_at_half_height() {
    let slab = state("minecraft:stone_slab[type=bottom,waterlogged=false]");
    let w = world(vec![((0, 64, 0), slab)]);
    let click = (BlockPos::new(0, 64, 0), BlockFace::Up, Vec3f { x: 0.5, y: 0.5, z: 0.5 });
    assert_eq!(
        place(0, click, 0.0, &w),
        CushionUse::Place { position: Vec3::new(0.5, 64.5, 0.5), yaw: 0.0, color: 0 }
    );
}

#[test]
fn only_the_top_face_places() {
    let w = world(vec![]);
    for face in [BlockFace::Down, BlockFace::North, BlockFace::South, BlockFace::West, BlockFace::East] {
        let click = (BlockPos::new(0, 64, 0), face, Vec3f { x: 0.5, y: 0.5, z: 0.0 });
        assert_eq!(place(0, click, 0.0, &w), CushionUse::Refused, "{face:?}");
    }
}

#[test]
fn a_click_with_nothing_under_it_is_refused() {
    let w = world(vec![]);
    // A top-face click on air at y 70: the cushion would hover over empty cells.
    assert_eq!(place(0, top_of(0, 70, 0), 0.0, &w), CushionUse::Refused);
}

#[test]
fn a_solid_cell_is_refused() {
    // Stone also fills the cell the cushion would occupy, so it is buried.
    let w = world(vec![((0, 65, 0), state("minecraft:stone"))]);
    assert_eq!(place(0, top_of(0, 64, 0), 0.0, &w), CushionUse::Refused);
}

#[test]
fn a_click_on_a_composter_rests_on_its_collider_when_the_ray_crosses_it() {
    let composter = state("minecraft:composter[level=0]");
    let w = world(vec![((0, 64, 0), composter)]);
    // The click lands inside the cavity, 0.18 up the cell. The ray from the eye
    // passes over the west wall's top (a 2/16 thick rim reaching y 65.0) on the
    // way, so the collider is what the segment meets first: 65.0, not 64.18.
    let clicked = BlockPos::new(0, 64, 0);
    let cursor = Vec3f { x: 0.5, y: 0.18, z: 0.5 };
    let eye = Some(Vec3::new(-0.5, 66.0, 0.5));
    let CushionUse::Place { position, .. } =
        use_cushion_item(0, clicked, BlockFace::Up, cursor, eye, 0.0, &w)
    else {
        panic!("a composter top accepts a cushion");
    };
    assert_eq!(position, Vec3::new(0.5, 65.0, 0.5));
    // Without an eye the plain click height is used.
    let CushionUse::Place { position, .. } = use_cushion_item(0, clicked, BlockFace::Up, cursor, None, 0.0, &w) else {
        panic!("a composter top accepts a cushion");
    };
    assert!((position.y - 64.18).abs() < 1e-6, "{position:?}");
    // A ray straight down the cavity meets no collider before the click: unchanged.
    let CushionUse::Place { position, .. } =
        use_cushion_item(0, clicked, BlockFace::Up, cursor, Some(Vec3::new(0.5, 66.0, 0.5)), 0.0, &w)
    else {
        panic!("a composter top accepts a cushion");
    };
    assert!((position.y - 64.18).abs() < 1e-6, "{position:?}");
}

#[test]
fn yaw_snaps_to_the_nearest_quarter_turn() {
    for (yaw, expected) in [
        (0.0, 0.0),
        (44.9, 0.0),
        (45.0, 90.0),
        (100.0, 90.0),
        (180.0, 180.0),
        (-90.0, 270.0),
        (270.0, 270.0),
        (359.0, 0.0),
    ] {
        assert_eq!(snapped_yaw(yaw), expected, "yaw {yaw}");
    }
}

#[test]
fn stack_colour_follows_the_item_unless_the_component_overrides_it() {
    assert_eq!(color_for_stack(&stack("minecraft:red_cushion")), Some(14));
    assert_eq!(color_for_stack(&stack("minecraft:light_blue_cushion")), Some(3));
    assert_eq!(color_for_stack(&stack("minecraft:red_wool")), None);
    let mut patched = stack("minecraft:red_cushion");
    patched.components = ItemComponents { cushion_color: Some("cyan".into()), ..ItemComponents::default() };
    assert_eq!(color_for_stack(&patched), Some(9));
    assert_eq!(item_for_color(14).to_string(), "minecraft:red_cushion");
    assert_eq!(item_for_color(8).to_string(), "minecraft:light_gray_cushion");
}

#[test]
fn a_second_cushion_in_the_same_spot_is_refused() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    let w = world(vec![]);
    let red = stack("minecraft:red_cushion");
    let go = |x: i32| {
        let click = top_of(x, 64, 0);
        apply_cushion_item(&red, click.0, click.1, click.2, None, 0.0, &w, &mobs)
    };
    let CushionApplied::Placed { entity_id, position, color, burned, .. } = go(0) else {
        panic!("the first cushion places");
    };
    assert_eq!((position, color, burned), (Vec3::new(0.5, 65.0, 0.5), 14, false));
    assert_eq!(mobs.with(|sim| sim.cushion_state(entity_id)), Some((position, 0.0, 14)));
    assert_eq!(go(0), CushionApplied::Refused);
    assert!(matches!(go(1), CushionApplied::Placed { .. }), "the next cell is free");
    assert_eq!(mobs.with(|sim| sim.cushion_count()), 2);
    let wool = stack("minecraft:red_wool");
    let click = top_of(5, 64, 0);
    assert_eq!(
        apply_cushion_item(&wool, click.0, click.1, click.2, None, 0.0, &w, &mobs),
        CushionApplied::NotACushion
    );
}

#[test]
fn fire_in_the_box_is_detected() {
    let fire = state("minecraft:fire[age=0,east=false,north=false,south=false,up=false,west=false]");
    let bb = Aabb::cushion_at(Vec3::new(0.5, 65.0, 0.5));
    assert!(!fire_in(&bb, &world(vec![])));
    assert!(fire_in(&bb, &world(vec![((0, 65, 0), fire)])));
}

#[test]
fn removing_the_support_breaks_it_on_the_101st_tick_and_drops_its_colour() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    let id = mobs.with(|sim| sim.spawn_cushion(Vec3::new(0.5, 65.0, 0.5), 0.0, 14));
    let supported = world(vec![]);
    let unsupported = |_x: i32, _y: i32, _z: i32| air_state();
    mobs.with(|sim| {
        for tick in 1..=100 {
            assert!(sim.plan_cushion_checks(&unsupported).is_empty(), "tick {tick}");
        }
        assert_eq!(sim.plan_cushion_checks(&unsupported), vec![id]);
        // With support the same tick count passes quietly.
        let other = sim.spawn_cushion(Vec3::new(4.5, 65.0, 0.5), 0.0, 3);
        for _ in 0..=101 {
            assert!(!sim.plan_cushion_checks(&supported).contains(&other));
        }
        let broken = sim.break_cushion(id, true).expect("the cushion exists");
        assert_eq!((broken.rider, broken.color), (None, 14));
        assert!(sim.cushion_state(id).is_none());
        assert_eq!(sim.dropped_items(), vec![("minecraft:red_cushion".to_owned(), 1)]);
    });
}

#[test]
fn creative_breaking_drops_nothing() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let id = sim.spawn_cushion(Vec3::new(0.5, 65.0, 0.5), 0.0, 5);
        assert!(sim.break_cushion(id, false).is_some());
        assert!(sim.dropped_items().is_empty());
        assert!(sim.break_cushion(id, false).is_none());
    });
}

#[test]
fn one_player_sits_and_a_sneaking_or_second_player_does_not() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let id = sim.spawn_cushion(Vec3::new(0.5, 65.0, 0.5), 0.0, 0);
        assert!(!sim.mount_cushion(id, 7, true), "sneaking does not sit");
        assert!(sim.mount_cushion(id, 7, false));
        assert_eq!(sim.cushion_rider(id), Some(7));
        assert_eq!(sim.cushion_ridden_by(7), Some(id));
        assert!(!sim.mount_cushion(id, 8, false), "occupied");
        assert!(!sim.mount_cushion(id, 7, false), "the rider clicking again does not re-sit");
        assert_eq!(sim.dismount_cushion_rider(7), Some(id));
        assert!(sim.mount_cushion(id, 8, false));
        let broken = sim.break_cushion(id, true).unwrap();
        assert_eq!(broken.rider, Some(8));
    });
}

#[test]
fn a_cushion_streams_its_colour_as_metadata() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let id = sim.spawn_cushion(Vec3::new(0.5, 65.0, 0.5), 180.0, 13);
        let snap = sim.snapshots().into_iter().find(|s| s.id == id).expect("streamed");
        assert_eq!(snap.entity_type.to_string(), "minecraft:cushion");
        assert_eq!(snap.rotation.yaw, 180.0);
        assert_eq!(snap.metadata, vec![crate::protocol::MetadataField::CushionColor(13)]);
    });
}

#[test]
fn a_seated_cushion_that_loses_its_support_breaks_and_queues_the_riders_ejection() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    let unsupported = |_x: i32, _y: i32, _z: i32| air_state();
    mobs.with(|sim| {
        let id = sim.spawn_cushion(Vec3::new(0.5, 65.0, 0.5), 0.0, 14);
        assert!(sim.mount_cushion(id, 7, false));
        for _ in 0..100 {
            assert!(sim.plan_cushion_checks(&unsupported).is_empty());
        }
        assert_eq!(sim.plan_cushion_checks(&unsupported), vec![id], "a rider does not exempt it");
        let broken = sim.break_cushion(id, true).expect("the cushion exists");
        assert_eq!(broken.rider, Some(7));
        assert_eq!(broken.position, Vec3::new(0.5, 65.0, 0.5));
        assert_eq!(sim.take_ejections_of(8), Vec::<i32>::new(), "another player is not told");
        assert_eq!(sim.take_ejections_of(7), vec![id]);
        assert!(sim.take_ejections_of(7).is_empty(), "drained once");
    });
}

#[test]
fn an_unseated_break_queues_no_ejection() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let id = sim.spawn_cushion(Vec3::new(0.5, 65.0, 0.5), 0.0, 1);
        sim.break_cushion(id, false);
        assert!(sim.take_ejections_of(7).is_empty());
    });
}

/// Two cushions of different colour, yaw and cell, as `(position, yaw, colour)`.
const SPECS: [((f64, f64, f64), f32, u8); 2] = [((12.5, 71.0, 8.5), 90.0, 14), ((-3.5, 64.5, -40.5), 180.0, 3)];

fn populated() -> MobHandle {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        for ((x, y, z), yaw, color) in SPECS {
            sim.spawn_cushion(Vec3::new(x, y, z), yaw, color);
        }
    });
    mobs
}

fn states_of(sim: &crate::mobs::MobSim<'_>) -> Vec<(Vec3, f32, u8, Uuid)> {
    let mut found: Vec<_> = sim
        .saved_entities()
        .into_iter()
        .filter(|saved| saved.id.path() == "cushion")
        .map(|saved| {
            let color = sim
                .snapshots()
                .into_iter()
                .find(|snap| snap.uuid == saved.uuid)
                .and_then(|snap| match snap.metadata.as_slice() {
                    [crate::protocol::MetadataField::CushionColor(color)] => Some(*color),
                    _ => None,
                })
                .expect("a saved cushion is live and streams its colour");
            (saved.pos, saved.rotation.yaw, color, saved.uuid)
        })
        .collect();
    found.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));
    found
}

/// The Anvil record carries the colour as a dye name and the cell as an int
/// array, and a reopened store gives back the same cushions with the same uuids.
#[test]
fn cushions_survive_a_save_to_the_entity_region_files_and_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::entity_storage::EntityStorage::new(dir.path()).unwrap();
    let before = populated();
    let (saved, expected) = before.with(|sim| (sim.saved_entities(), states_of(sim)));
    assert_eq!(expected.len(), 2);
    let red = saved.iter().find(|s| s.pos.x == 12.5).unwrap();
    assert_eq!(
        red.extra,
        vec![
            ("color".to_owned(), lodestone_core::Nbt::String("red".to_owned())),
            ("block_pos".to_owned(), lodestone_core::Nbt::IntArray(vec![12, 71, 8])),
        ]
    );
    storage.save(&saved).unwrap();

    let loaded = storage.load_all().unwrap();
    assert_eq!(loaded.iter().filter(|s| s.id.path() == "cushion").count(), 2);
    let after = MobHandle::new(ChunkWorld::new(-64, 384));
    assert_eq!(after.with(|sim| sim.restore_saved(&loaded)), 2);
    assert_eq!(after.with(|sim| states_of(sim)), expected);
    // A restored cushion is a working one: it seats a player.
    after.with(|sim| {
        let id = sim.snapshots().into_iter().find(|s| s.position.x == 12.5).unwrap().id;
        assert!(sim.mount_cushion(id, 7, false));
    });
}

#[test]
fn cushions_survive_a_save_to_the_native_store_and_a_reload() {
    let before = populated();
    let (records, expected) = before.with(|sim| {
        (sim.native_entities(lodestone_storage_schema::BuiltinDimension::Overworld), states_of(sim))
    });
    assert_eq!(records.len(), 2, "a cushion's colour is state, so the record is kept");
    let after = MobHandle::new(ChunkWorld::new(-64, 384));
    assert_eq!(after.with(|sim| sim.restore_native(&records)), 2);
    assert_eq!(after.with(|sim| states_of(sim)), expected);
}

/// A cushion broken after a save must not come back: its stored record is
/// removed by the owner set, the same way a despawned mob's is.
#[test]
fn a_cushion_broken_after_a_save_is_gone_after_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let stores = crate::entity_storage::DimensionEntityStores::new(dir.path()).unwrap();
    let overworld = crate::dimension::Dimension::Overworld;
    let mobs = populated();
    stores.save_live(overworld, &mobs.with(|sim| sim.saved_entities())).unwrap();
    mobs.with(|sim| {
        let id = sim.snapshots().into_iter().find(|s| s.position.x == 12.5).unwrap().id;
        sim.break_cushion(id, false);
    });
    stores.save_live(overworld, &mobs.with(|sim| sim.saved_entities())).unwrap();
    let loaded = stores.storage(overworld).load_all().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].pos.x, -3.5);
}

#[test]
fn cushion_sounds_and_particles_use_the_reference_events() {
    use crate::effects::{CushionSound, WorldEffect, cushion_break_particles, cushion_sound};
    use lodestone_model::SoundCategory;

    let at = Vec3::new(2.5, 65.0, 3.5);
    let sound = |kind| match cushion_sound(kind, at, 9) {
        Some(WorldEffect::Sound { sound, category, volume, pitch, pos, .. }) => (sound, category, volume, pitch, pos),
        other => panic!("expected a sound, got {other:?}"),
    };
    assert_eq!(
        sound(CushionSound::Place),
        ("minecraft:entity.cushion.place".to_owned(), SoundCategory::Block, 0.75, 0.8, at)
    );
    assert_eq!(sound(CushionSound::Sit).0, "minecraft:entity.cushion.sit");
    assert_eq!(sound(CushionSound::GetUp).0, "minecraft:entity.cushion.get_up");
    assert_eq!(
        sound(CushionSound::Break),
        ("minecraft:entity.cushion.break".to_owned(), SoundCategory::Neutral, 1.0, 1.0, at)
    );

    // Red wool, ten of them, two thirds of the way up the quarter-block box.
    let Some(WorldEffect::BlockParticles { state, pos, offset, max_speed, count }) = cushion_break_particles(at, 14)
    else {
        panic!("a break throws particles");
    };
    assert_eq!(state, StateId::from_state_str("minecraft:red_wool").unwrap());
    assert_eq!((count, max_speed), (10, 0.05));
    assert!((pos.y - (65.0 + 1.0 / 6.0)).abs() < 1e-9);
    assert_eq!((offset.x, offset.y, offset.z), (0.25, 0.0625, 0.25));
    assert_ne!(
        cushion_break_particles(at, 3).map(|effect| format!("{effect:?}")),
        cushion_break_particles(at, 14).map(|effect| format!("{effect:?}")),
        "the colour picks the wool"
    );
}
