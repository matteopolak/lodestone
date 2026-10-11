//! Expected values are worked from the reference rules by hand: the box centre is the
//! cell centre moved 0.46875 towards the wall, the slab is 1/16 thick and 0.75 square (a
//! block across with a map), horizontal facings turn by quarter turns from south, and
//! a support check fires on the 101st tick.

use lodestone_core::Nbt;
use lodestone_data::block_states::{StateId, air_state};
use lodestone_model::{BlockFace, BlockPos, ItemStack, ResourceKey, Vec3};

use super::*;
use crate::mobs::{ChunkWorld, FrameHit, FrameInteraction, MobHandle};

fn state(name: &str) -> StateId {
    StateId::from_state_str(name).expect("fixture state is registered")
}

/// Stone at the cells in `walls`, air elsewhere.
fn walls(walls: Vec<(i32, i32, i32)>) -> impl Fn(i32, i32, i32) -> StateId {
    let stone = state("minecraft:stone");
    move |x, y, z| if walls.contains(&(x, y, z)) { stone } else { air_state() }
}

fn stack(item: &str, count: u32) -> ItemStack {
    ItemStack::new(item.parse::<ResourceKey>().unwrap(), count)
}

fn map_stack(id: i32) -> ItemStack {
    let mut s = stack("minecraft:filled_map", 1);
    s.components.map_id = Some(id);
    s
}

const CELL: BlockPos = BlockPos { x: 6, y: 65, z: 3 };

fn near(a: [f64; 3], b: [f64; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() < 1e-9)
}

#[test]
fn the_box_is_a_thin_slab_on_the_wall_side_of_the_cell() {
    // East-facing at (6, 65, 3): the wall is the west side of the cell, x = 6.0.
    let bare = bounding_box(CELL, BlockFace::East, false);
    assert!(near(bare.min, [6.0, 65.125, 3.125]), "{bare:?}");
    assert!(near(bare.max, [6.0625, 65.875, 3.875]), "{bare:?}");
    // A map fills the block across, the thickness unchanged.
    let mapped = bounding_box(CELL, BlockFace::East, true);
    assert!(near(mapped.min, [6.0, 65.0, 3.0]) && near(mapped.max, [6.0625, 66.0, 4.0]), "{mapped:?}");
    // The survival box stays bare.
    assert_eq!(pop_box(CELL, BlockFace::East), bare);
    // North-facing: the wall is the south side, z = 4.0.
    let north = bounding_box(CELL, BlockFace::North, false);
    assert!(near(north.min, [6.125, 65.125, 3.9375]) && near(north.max, [6.875, 65.875, 4.0]), "{north:?}");
    // A floor frame hugs the cell's bottom, a ceiling frame its top.
    let up = bounding_box(CELL, BlockFace::Up, false);
    assert!(near(up.min, [6.125, 65.0, 3.125]) && near(up.max, [6.875, 65.0625, 3.875]), "{up:?}");
    let down = bounding_box(CELL, BlockFace::Down, false);
    assert!(near(down.min, [6.125, 65.9375, 3.125]) && near(down.max, [6.875, 66.0, 3.875]), "{down:?}");
}

#[test]
fn the_wire_value_yaw_and_pitch_follow_the_direction_tables() {
    // Down 0, up 1, north 2, south 3, west 4, east 5; south 0, west 1, north 2, east 3 by quarter turns.
    for (face, wire, yaw, pitch) in [
        (BlockFace::Down, 0, 0.0, 90.0),
        (BlockFace::Up, 1, 0.0, -90.0),
        (BlockFace::North, 2, 180.0, 0.0),
        (BlockFace::South, 3, 0.0, 0.0),
        (BlockFace::West, 4, 90.0, 0.0),
        (BlockFace::East, 5, 270.0, 0.0),
    ] {
        assert_eq!(data_3d(face), wire);
        assert_eq!(from_data_3d(i32::from(wire)), Some(face));
        assert_eq!(yaw_pitch(face), (yaw, pitch), "{face:?}");
    }
    assert_eq!(from_data_3d(6), None);
    assert_eq!(anchor(CELL), Vec3::new(6.0, 65.0, 3.0), "the spawn packet carries the cell corner");
    assert_eq!(centre(CELL, BlockFace::East), Vec3::new(6.03125, 65.5, 3.5));
}

#[test]
fn a_frame_needs_a_solid_wall_and_free_air() {
    let wall = (5, 65, 3);
    let on_wall = walls(vec![wall]);
    assert!(survives(CELL, BlockFace::East, &on_wall, &[]));
    assert!(!survives(CELL, BlockFace::East, &walls(vec![]), &[]), "nothing behind it");
    assert!(
        !survives(CELL, BlockFace::East, &|x, y, z| if (x, y, z) == wall { state("minecraft:torch") } else { air_state() }, &[]),
        "a torch is not a wall"
    );
    // Stone filling the frame's own cell collides with its slab.
    assert!(!survives(CELL, BlockFace::East, &walls(vec![wall, (6, 65, 3)]), &[]));
    // A fence post beside the cell, outside the slab, does not.
    assert!(survives(CELL, BlockFace::East, &walls(vec![wall, (6, 65, 4)]), &[]));
}

#[test]
fn a_repeater_holds_a_wall_frame_but_not_a_floor_frame() {
    let repeater = state("minecraft:repeater[delay=1,facing=north,locked=false,powered=false]");
    let w = |x: i32, y: i32, z: i32| if (x, y, z) == (5, 65, 3) || (x, y, z) == (6, 64, 3) { repeater } else { air_state() };
    assert!(survives(CELL, BlockFace::East, &w, &[]));
    assert!(!survives(CELL, BlockFace::Up, &w, &[]), "a floor frame needs a solid block");
}

#[test]
fn two_frames_facing_the_same_way_cannot_overlap_but_opposite_ones_can() {
    let both = walls(vec![(5, 65, 3), (7, 65, 3)]);
    let east = Neighbour { cell: CELL, facing: BlockFace::East, has_map: false };
    assert!(!survives(CELL, BlockFace::East, &both, &[east]));
    let west = Neighbour { cell: CELL, facing: BlockFace::West, has_map: false };
    assert!(survives(CELL, BlockFace::East, &both, &[west]));
    // A neighbouring cell's mapped frame (a block across) does not reach this cell's slab.
    let beside = Neighbour { cell: BlockPos::new(6, 65, 4), facing: BlockFace::East, has_map: true };
    assert!(survives(CELL, BlockFace::East, &both, &[beside]));
}

#[test]
fn the_item_places_on_a_wall_face_for_a_builder_inside_the_height() {
    let w = walls(vec![(5, 65, 3)]);
    let range = (-64, 320);
    let clicked = BlockPos::new(5, 65, 3);
    assert_eq!(
        use_frame_item(clicked, BlockFace::East, true, range, &w, &[]),
        FrameUse::Place { cell: CELL, facing: BlockFace::East }
    );
    assert_eq!(use_frame_item(clicked, BlockFace::East, false, range, &w, &[]), FrameUse::Refused, "adventure");
    assert_eq!(use_frame_item(clicked, BlockFace::East, true, (66, 320), &w, &[]), FrameUse::Refused, "below range");
    assert_eq!(use_frame_item(clicked, BlockFace::East, true, (-64, 65), &w, &[]), FrameUse::Refused, "above range");
    assert_eq!(use_frame_item(clicked, BlockFace::North, true, range, &w, &[]), FrameUse::Place {
        cell: BlockPos::new(5, 65, 2),
        facing: BlockFace::North,
    });
    // A floor top works as well: frames are not limited to walls.
    let floor = walls(vec![(5, 64, 3)]);
    assert_eq!(
        use_frame_item(BlockPos::new(5, 64, 3), BlockFace::Up, true, range, &floor, &[]),
        FrameUse::Place { cell: BlockPos::new(5, 65, 3), facing: BlockFace::Up }
    );
    assert_eq!(glow_of_item(&stack("minecraft:item_frame", 1)), Some(false));
    assert_eq!(glow_of_item(&stack("minecraft:glow_item_frame", 1)), Some(true));
    assert_eq!(glow_of_item(&stack("minecraft:painting", 1)), None);
}

#[test]
fn applying_the_item_spawns_one_frame_and_a_second_in_the_cell_is_refused() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    let w = walls(vec![(5, 65, 3)]);
    let apply = |item: &str| {
        apply_frame_item(&stack(item, 1), BlockPos::new(5, 65, 3), BlockFace::East, true, (-64, 320), &w, &mobs)
    };
    assert_eq!(apply("minecraft:stone"), FrameApplied::NotAFrame);
    let FrameApplied::Placed { centre: c, glow, .. } = apply("minecraft:glow_item_frame") else { panic!("placed") };
    assert!(glow);
    assert_eq!(c, Vec3::new(6.03125, 65.5, 3.5));
    assert_eq!(apply("minecraft:item_frame"), FrameApplied::Refused, "the cell is taken");
    assert_eq!(mobs.with(|sim| sim.frame_count()), 1);
}

fn one_frame(sim: &mut crate::mobs::MobSim<'_>) -> i32 {
    sim.spawn_item_frame(CELL, BlockFace::East, false)
}

#[test]
fn an_empty_frame_takes_one_item_and_a_full_one_turns_it_through_eight_steps() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let id = one_frame(sim);
        let never = |_: i32| false;
        assert_eq!(sim.interact_frame(id, None, &never), FrameInteraction::Pass, "empty hand, empty frame");
        let c = Vec3::new(6.03125, 65.5, 3.5);
        assert_eq!(
            sim.interact_frame(id, Some(&stack("minecraft:diamond", 5)), &never),
            FrameInteraction::Inserted { glow: false, centre: c }
        );
        let (_, _, item, rotation) = sim.frame_state(id).unwrap();
        assert_eq!((item.map(|s| (s.item.to_string(), s.count)), rotation), (Some(("minecraft:diamond".to_owned(), 1)), 0));
        for expected in [1, 2, 3, 4, 5, 6, 7, 0] {
            assert_eq!(sim.interact_frame(id, None, &never), FrameInteraction::Rotated { glow: false, centre: c });
            assert_eq!(sim.frame_state(id).unwrap().3, expected);
        }
        // A held item does not replace the framed one.
        sim.interact_frame(id, Some(&stack("minecraft:stick", 1)), &never);
        assert_eq!(sim.frame_state(id).unwrap().2.unwrap().item.to_string(), "minecraft:diamond");
    });
}

#[test]
fn a_map_over_its_marker_limit_is_refused() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let id = one_frame(sim);
        assert_eq!(sim.interact_frame(id, Some(&map_stack(4)), &|_| true), FrameInteraction::Refused);
        assert!(sim.frame_state(id).unwrap().2.is_none());
        assert!(matches!(sim.interact_frame(id, Some(&map_stack(4)), &|_| false), FrameInteraction::Inserted { .. }));
    });
}

#[test]
fn a_hit_pops_the_item_then_breaks_the_frame() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let id = one_frame(sim);
        sim.interact_frame(id, Some(&map_stack(9)), &|_| false);
        let Some(FrameHit::ItemPopped { map, .. }) = sim.hurt_frame(id, false) else { panic!("pops") };
        assert_eq!(map.map(|m| (m.map_id, m.cell, m.entity_id)), Some((9, CELL, id)));
        assert!(sim.is_item_frame(id), "the frame stays");
        assert_eq!(sim.dropped_items(), vec![("minecraft:filled_map".to_owned(), 1)]);
        let Some(FrameHit::Broken(broken)) = sim.hurt_frame(id, false) else { panic!("breaks") };
        assert_eq!(broken.map, None, "the map left with the pop");
        assert!(!sim.is_item_frame(id));
        let mut dropped = sim.dropped_items();
        dropped.sort();
        assert_eq!(
            dropped,
            vec![("minecraft:filled_map".to_owned(), 1), ("minecraft:item_frame".to_owned(), 1)]
        );
        assert!(sim.hurt_frame(id, false).is_none());
    });
}

#[test]
fn a_creative_hit_drops_nothing_and_a_fixed_frame_yields_only_to_creative() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let id = one_frame(sim);
        sim.interact_frame(id, Some(&stack("minecraft:diamond", 1)), &|_| false);
        assert!(matches!(sim.hurt_frame(id, true), Some(FrameHit::ItemPopped { .. })));
        assert!(matches!(sim.hurt_frame(id, true), Some(FrameHit::Broken(_))));
        assert!(sim.dropped_items().is_empty());

        // A fixed frame, restored from a record carrying Fixed.
        let mut saved = sim_saved(sim, BlockFace::East);
        saved.extra.push(("Fixed".to_owned(), Nbt::Byte(1)));
        saved.extra.retain(|(k, v)| !(k == "Fixed" && *v == Nbt::Byte(0)));
        sim.restore_saved(&[saved]);
        let fixed = sim.snapshots().into_iter().find(|s| s.entity_type.path() == "item_frame").unwrap().id;
        assert_eq!(sim.interact_frame(fixed, Some(&stack("minecraft:diamond", 1)), &|_| false), FrameInteraction::Pass);
        assert_eq!(sim.hurt_frame(fixed, false), Some(FrameHit::Ignored));
        assert!(matches!(sim.hurt_frame(fixed, true), Some(FrameHit::Broken(_))));
        assert!(sim.dropped_items().is_empty());
    });
}

/// A saved record for an empty frame facing `facing` in `CELL`.
fn sim_saved(sim: &mut crate::mobs::MobSim<'_>, facing: BlockFace) -> crate::entity_record::SavedEntity {
    let id = sim.spawn_item_frame(CELL, facing, false);
    let saved = sim.saved_entities().into_iter().find(|s| s.id.path() == "item_frame").unwrap();
    sim.kill_frame(id, false);
    saved
}

#[test]
fn the_support_check_fires_on_the_101st_tick() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    let none = |_x: i32, _y: i32, _z: i32| air_state();
    let wall = walls(vec![(5, 65, 3)]);
    mobs.with(|sim| {
        let id = one_frame(sim);
        for _ in 0..100 {
            assert!(sim.plan_frame_checks(&none).is_empty());
        }
        assert_eq!(sim.plan_frame_checks(&none), vec![id]);
        let broken = sim.kill_frame(id, true).unwrap();
        assert_eq!(broken.centre, Vec3::new(6.03125, 65.5, 3.5));
        assert_eq!(sim.dropped_items(), vec![("minecraft:item_frame".to_owned(), 1)]);
        let kept = one_frame(sim);
        for _ in 0..101 {
            assert!(sim.plan_frame_checks(&wall).is_empty(), "a supported frame stays");
        }
        assert!(sim.is_item_frame(kept));
    });
}

#[test]
fn a_frame_streams_its_facing_item_and_rotation() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let id = one_frame(sim);
        let snap = |sim: &crate::mobs::MobSim<'_>| sim.snapshots().into_iter().find(|s| s.id == id).unwrap();
        let first = snap(sim);
        assert_eq!(first.entity_type.to_string(), "minecraft:item_frame");
        assert_eq!(first.position, Vec3::new(6.0, 65.0, 3.0));
        assert_eq!((first.rotation.yaw, first.rotation.pitch), (270.0, 0.0));
        assert_eq!(first.object_data, 5, "the spawn data is the facing's wire value");
        let empty = crate::protocol::MetadataField::FrameItem {
            item: "minecraft:air".parse().unwrap(),
            count: 0,
            components: None,
        };
        assert_eq!(
            first.metadata,
            vec![crate::protocol::MetadataField::HangingFacing(5), empty, crate::protocol::MetadataField::FrameRotation(0)]
        );
        sim.interact_frame(id, Some(&map_stack(3)), &|_| false);
        sim.interact_frame(id, None, &|_| false);
        let after = snap(sim);
        let [_, crate::protocol::MetadataField::FrameItem { item, count, components }, crate::protocol::MetadataField::FrameRotation(r)] =
            after.metadata.as_slice()
        else {
            panic!("three fields")
        };
        assert_eq!((item.to_string(), *count, *r), ("minecraft:filled_map".to_owned(), 1, 1));
        assert_eq!(components.as_ref().and_then(|c| c.map_id), Some(3));
    });
}

#[test]
fn frames_holding_maps_are_reported_with_their_marker_rotation() {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let a = sim.spawn_item_frame(CELL, BlockFace::East, false);
        let b = sim.spawn_item_frame(BlockPos::new(0, 70, 0), BlockFace::Up, true);
        let c = sim.spawn_item_frame(BlockPos::new(9, 70, 9), BlockFace::North, true);
        for (id, map) in [(a, 1), (b, 2)] {
            sim.interact_frame(id, Some(&map_stack(map)), &|_| false);
        }
        sim.interact_frame(c, Some(&stack("minecraft:diamond", 1)), &|_| false);
        let maps = sim.framed_maps();
        assert_eq!(
            maps.iter().map(|m| (m.map_id, m.pos, m.rotation)).collect::<Vec<_>>(),
            vec![(1, CELL, 270), (2, BlockPos::new(0, 70, 0), -90)]
        );
    });
}

fn populated() -> MobHandle {
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    mobs.with(|sim| {
        let a = sim.spawn_item_frame(CELL, BlockFace::East, false);
        sim.interact_frame(a, Some(&stack("minecraft:diamond", 1)), &|_| false);
        for _ in 0..3 {
            sim.interact_frame(a, None, &|_| false);
        }
        let b = sim.spawn_item_frame(BlockPos::new(-20, 70, 40), BlockFace::Down, true);
        sim.interact_frame(b, Some(&map_stack(12)), &|_| false);
    });
    mobs
}

type Seen = Vec<(String, Vec3, f32, f32, i32, Option<(String, Option<i32>)>, u8, uuid::Uuid)>;

fn seen(sim: &crate::mobs::MobSim<'_>) -> Seen {
    let mut found: Seen = sim
        .snapshots()
        .into_iter()
        .filter(|s| s.entity_type.path().ends_with("item_frame"))
        .map(|s| {
            let crate::protocol::MetadataField::FrameItem { item, components, .. } = &s.metadata[1] else {
                panic!("item field")
            };
            let crate::protocol::MetadataField::FrameRotation(rotation) = s.metadata[2] else { panic!("rotation") };
            (
                s.entity_type.to_string(),
                s.position,
                s.rotation.yaw,
                s.rotation.pitch,
                s.object_data,
                (item.path() != "air").then(|| (item.to_string(), components.as_ref().and_then(|c| c.map_id))),
                rotation,
                s.uuid,
            )
        })
        .collect();
    found.sort_by(|a, b| a.1.x.total_cmp(&b.1.x));
    found
}

#[test]
fn frames_survive_a_save_to_the_entity_region_files_and_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::entity_storage::EntityStorage::new(dir.path()).unwrap();
    let before = populated();
    let (saved, expected) = before.with(|sim| (sim.saved_entities(), seen(sim)));
    assert_eq!(expected.len(), 2);
    let diamond = saved.iter().find(|s| s.id.path() == "item_frame").unwrap();
    // The record is the reference's: the box centre as the position, the item under `Item`.
    assert_eq!(diamond.pos, Vec3::new(6.03125, 65.5, 3.5));
    assert_eq!(diamond.item.as_ref().map(|s| s.item.to_string()), Some("minecraft:diamond".to_owned()));
    for (key, value) in [
        ("ItemRotation", Nbt::Byte(3)),
        ("Facing", Nbt::Byte(5)),
        ("Fixed", Nbt::Byte(0)),
        ("block_pos", Nbt::IntArray(vec![6, 65, 3])),
    ] {
        assert_eq!(diamond.extra.iter().find(|(k, _)| k == key).map(|(_, v)| v), Some(&value), "{key}");
    }
    storage.save(&saved).unwrap();

    let loaded = storage.load_all().unwrap();
    let after = MobHandle::new(ChunkWorld::new(-64, 384));
    assert_eq!(after.with(|sim| sim.restore_saved(&loaded)), 2);
    assert_eq!(after.with(|sim| seen(sim)), expected);
}

#[test]
fn frames_survive_a_save_to_the_native_store_and_a_reload() {
    let before = populated();
    let (records, expected) = before.with(|sim| {
        (sim.native_entities(lodestone_storage_schema::BuiltinDimension::Overworld), seen(sim))
    });
    assert_eq!(records.len(), 2);
    let after = MobHandle::new(ChunkWorld::new(-64, 384));
    assert_eq!(after.with(|sim| sim.restore_native(&records)), 2);
    assert_eq!(after.with(|sim| seen(sim)), expected);
}

#[test]
fn a_frame_broken_after_a_save_is_gone_after_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let stores = crate::entity_storage::DimensionEntityStores::new(dir.path()).unwrap();
    let overworld = crate::dimension::Dimension::Overworld;
    let mobs = populated();
    stores.save_live(overworld, &mobs.with(|sim| sim.saved_entities())).unwrap();
    mobs.with(|sim| {
        let id = sim.snapshots().into_iter().find(|s| s.position.x == 6.0).unwrap().id;
        sim.kill_frame(id, false);
    });
    stores.save_live(overworld, &mobs.with(|sim| sim.saved_entities())).unwrap();
    let loaded = stores.storage(overworld).load_all().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id.path(), "glow_item_frame");
}

#[test]
fn frame_sounds_use_the_reference_events_in_the_neutral_category() {
    use crate::effects::{WorldEffect, item_frame_sound};
    use lodestone_model::SoundCategory;
    let at = Vec3::new(1.0, 2.0, 3.0);
    for (kind, event) in [
        (FrameSound::Place, "place"),
        (FrameSound::AddItem, "add_item"),
        (FrameSound::Rotate, "rotate_item"),
        (FrameSound::RemoveItem, "remove_item"),
        (FrameSound::Break, "break"),
    ] {
        for (glow, entity) in [(false, "item_frame"), (true, "glow_item_frame")] {
            let Some(WorldEffect::Sound { sound, category, volume, pitch, pos, .. }) = item_frame_sound(kind, glow, at, 1)
            else {
                panic!("{entity} {event} exists")
            };
            assert_eq!(sound, format!("minecraft:entity.{entity}.{event}"));
            assert_eq!((category, volume, pitch, pos), (SoundCategory::Neutral, 1.0, 1.0, at));
        }
    }
}
