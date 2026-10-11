//! Expected values are worked from the reference rules by hand: pixel `(px, py)` of a map centred
//! on the origin at scale 0 is block `(px - 64, py - 64)`, a sampling pass handles the map columns
//! whose index is congruent to the holder's step modulo 16, the packed pixel is `colour << 2 | shade`
//! with colours read from the palette definition (grass 1, stone 11, water 12), and shades follow the
//! height-step and water-depth thresholds.

use lodestone_model::{BlockPos, ResourceKey};

use super::*;

fn state(text: &str) -> StateId {
    StateId::from_state_str(text).expect("fixture state is registered")
}

struct Grid<F: Fn(i32, i32, i32) -> StateId>(F);

impl<F: Fn(i32, i32, i32) -> StateId> MapTerrain for Grid<F> {
    fn is_loaded(&self, _x: i32, _z: i32) -> bool {
        true
    }
    fn min_y(&self) -> i32 {
        -64
    }
    fn surface_top(&self, x: i32, z: i32) -> i32 {
        (-64..320).rev().find(|&y| (self.0)(x, y, z) != StateId::AIR).map_or(-64, |y| y + 1)
    }
    fn state(&self, x: i32, y: i32, z: i32) -> StateId {
        (self.0)(x, y, z)
    }
}

fn carrier(uuid: Uuid, x: f64, z: f64, yaw: f32, dimension: Dimension) -> Carrier<'static> {
    Carrier { uuid, name: "Mapper", x, z, yaw, dimension, game_time: 0 }
}

/// Grass at y 64 over stone, air above.
fn flat(x: i32, y: i32, z: i32) -> StateId {
    let _ = (x, z);
    match y {
        ..=63 => state("minecraft:stone"),
        64 => state("minecraft:grass_block[snowy=false]"),
        _ => StateId::AIR,
    }
}

const GRASS_NORMAL: u8 = (1 << 2) | 1;

fn overworld_map() -> MapData {
    MapData::fresh(0.0, 0.0, 0, true, false, "minecraft:overworld")
}

fn sampled(map: &mut MapData, who: &Carrier<'_>, terrain: &dyn MapTerrain, ceiling: bool) {
    map.tick_carried_by(who);
    map.sample_terrain(who, terrain, ceiling);
}

#[test]
fn a_fresh_map_centres_on_the_grid_of_its_scale() {
    // size = 128 << scale; area = floor((p + 64) / size); centre = area * size + size / 2 - 64.
    assert_eq!(MapData::fresh(0.0, 0.0, 0, true, false, "x").center_x, 0);
    assert_eq!(MapData::fresh(100.0, 0.0, 0, true, false, "x").center_x, 128);
    assert_eq!(MapData::fresh(-65.0, 63.0, 0, true, false, "x").center_x, -128);
    assert_eq!(MapData::fresh(-65.0, 63.0, 0, true, false, "x").center_z, 0);
    // scale 2: size 512; 300 -> area 0 -> 0 + 256 - 64.
    assert_eq!(MapData::fresh(300.0, -300.0, 2, true, false, "x").center_x, 192);
    // -300 + 64 = -236 -> area -1 -> -512 + 192.
    assert_eq!(MapData::fresh(300.0, -300.0, 2, true, false, "x").center_z, -320);
}

#[test]
fn a_pass_over_flat_grass_chains_across_changed_columns_then_goes_quiet() {
    let mut map = overworld_map();
    let who = carrier(Uuid::from_u128(1), 0.5, 0.5, 0.0, Dimension::Overworld);
    sampled(&mut map, &who, &Grid(flat), false);
    // Step 1 starts at the first column whose index is 1 mod 16; every column it changes drags the
    // next one in, so the pass runs to the right edge. Column 0 is never reached.
    for x in 0..MAP_SIZE {
        for y in 0..MAP_SIZE {
            assert_eq!(map.color_at(x, y), if x == 0 { 0 } else { GRASS_NORMAL }, "pixel ({x}, {y})");
        }
    }
    // Later passes change nothing, so each handles only its own 1-in-16 columns: a stone column at
    // pixel 20 (block -44) is not seen at steps 2 and 3, and is at step 4 (20 mod 16).
    let stone_at_20 = Grid(|x, y, z| if x == -44 && y == 64 { state("minecraft:stone") } else { flat(x, y, z) });
    for _ in 0..2 {
        map.sample_terrain(&who, &stone_at_20, false);
        assert_eq!(map.color_at(20, 10), GRASS_NORMAL);
    }
    map.sample_terrain(&who, &stone_at_20, false);
    assert_eq!(map.color_at(20, 10), (11 << 2) | 1);
    assert_eq!(map.color_at(21, 10), GRASS_NORMAL);
}

#[test]
fn a_rise_shades_high_and_the_drop_after_it_shades_low() {
    // A stone pillar two above the grass at block (-63, -59), pixel (1, 5).
    let terrain = Grid(|x, y, z| {
        if (x, z) == (-63, -59) && (65..=66).contains(&y) { state("minecraft:stone") } else { flat(x, y, z) }
    });
    let mut map = overworld_map();
    sampled(&mut map, &carrier(Uuid::from_u128(1), 0.5, 0.5, 0.0, Dimension::Overworld), &terrain, false);
    // height step +2 over scale 1: 2 * 4 / 5 = 1.6, parity of 1 + 5 even: -0.2 -> 1.4 > 0.6.
    assert_eq!(map.color_at(1, 5), (11 << 2) | 2);
    // the grass row below: step -2 -> -1.6, parity of 1 + 6 odd: +0.2 -> -1.4 < -0.6.
    assert_eq!(map.color_at(1, 6), 1 << 2);
    // the row above is untouched flat ground.
    assert_eq!(map.color_at(1, 4), GRASS_NORMAL);
}

#[test]
fn water_shades_by_depth_and_parity() {
    // Four water blocks (y 61..=64) over dirt at y 60, at pixels (1, 4) and (1, 5).
    let terrain = Grid(|x, y, z| {
        if x == -63 && (-60..=-59).contains(&z) {
            return match y {
                ..=59 => state("minecraft:stone"),
                60 => state("minecraft:dirt"),
                61..=64 => state("minecraft:water[level=0]"),
                _ => StateId::AIR,
            };
        }
        flat(x, y, z)
    });
    let mut map = overworld_map();
    sampled(&mut map, &carrier(Uuid::from_u128(1), 0.5, 0.5, 0.0, Dimension::Overworld), &terrain, false);
    // depth 4 -> 0.4 plus 0.2 on odd (x + y): pixel (1, 4) is odd -> 0.6 normal; (1, 5) even -> 0.4 high.
    assert_eq!(map.color_at(1, 4), (12 << 2) | 1);
    assert_eq!(map.color_at(1, 5), (12 << 2) | 2);
}

#[test]
fn the_rim_dithers_and_the_radius_cuts_off() {
    // scale 4: radius 128 / 16 = 8, dithering past (8 - 2)^2 = 36, nothing at or past 64.
    let mut map = MapData::bare("minecraft:overworld", 0, 0, 4, true, false, false);
    sampled(&mut map, &carrier(Uuid::from_u128(1), 0.5, 0.5, 0.0, Dimension::Overworld), &Grid(flat), false);
    // The only selected column is 65 (65 & 15 == 1). Pixel (65, 64 + dy): distance^2 = 1 + dy^2.
    let painted = |dy: i32| map.color_at(65, (64 + dy) as usize) != 0;
    assert!(painted(5), "26 is inside the undithered disc");
    assert!(painted(6), "37 is dithered and (65 + 70) is odd");
    assert!(!painted(7), "50 is dithered and (65 + 71) is even");
    assert!(!painted(8), "65 is outside the radius");
    assert!(!painted(-8));
}

#[test]
fn a_ceiling_dimension_halves_the_radius_and_paints_dirt_or_stone_noise() {
    let mut map = MapData::bare("minecraft:the_nether", 0, 0, 0, true, false, false);
    let who = carrier(Uuid::from_u128(1), 0.5, 0.5, 0.0, Dimension::Nether);
    sampled(&mut map, &who, &Grid(|_, _, _| StateId::AIR), true);
    // radius 64: columns 1, 17, 33, 49, 65, 81 (the range is 64 - 64 + 1 ..= 127).
    assert_eq!(map.color_at(0, 64), 0);
    let value = map.color_at(1, 64);
    // noise: n = x + z * 231871 for x = -63, z = 0; n * n * 31287121 + n * 11, bit 20.
    let n: i32 = -63;
    let noise = n.wrapping_mul(n).wrapping_mul(31_287_121).wrapping_add(n.wrapping_mul(11));
    let colour = if (noise >> 20) & 1 == 0 { 10 } else { 11 };
    assert_eq!(value >> 2, colour);
    assert_eq!(map.color_at(1, 0), 0, "pixel (1, 0) is outside the halved radius, inside the full one");
}

#[test]
fn player_markers_follow_position_and_facing() {
    let mut map = overworld_map();
    let mut mark = |x: f64, z: f64, yaw: f32| {
        map.tick_carried_by(&carrier(Uuid::from_u128(1), x, z, yaw, Dimension::Overworld));
        map.decorations().map(|(key, d)| (key.to_owned(), d.clone())).collect::<Vec<_>>()
    };
    // (10.5, -20.25): 10.5 * 2 + 0.5 = 21.5 -> 21; -20.25 * 2 + 0.5 = -40.0 -> -40; yaw 0 -> 8 * 16 / 360 -> 0.
    let marks = mark(10.5, -20.25, 0.0);
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].0, "Mapper");
    assert_eq!((marks[0].1.kind.as_str(), marks[0].1.x, marks[0].1.y, marks[0].1.rotation), ("player", 21, -40, 0));
    // facing north (yaw 180) -> 188 * 16 / 360 = 8.35 -> 8; facing east (yaw -90) -> -98 * 16 / 360 = -4.35 -> -4 -> 12.
    assert_eq!(mark(0.0, 0.0, 180.0)[0].1.rotation, 8);
    assert_eq!(mark(0.0, 0.0, -90.0)[0].1.rotation, 12);
    // 100 blocks east is past the 63 limit but inside 320: the off-map marker, clamped to the edge.
    let off = &mark(100.0, 0.0, 0.0)[0].1;
    assert_eq!((off.kind.as_str(), off.x, off.y), ("player_off_map", 127, 0));
    // 400 blocks out is past the limit and the map does not track without limit: no marker.
    assert!(mark(400.0, 0.0, 0.0).is_empty());
}

#[test]
fn unlimited_tracking_keeps_a_marker_past_the_limit() {
    let mut map = MapData::fresh(0.0, 0.0, 0, true, true, "minecraft:overworld");
    map.tick_carried_by(&carrier(Uuid::from_u128(1), -400.0, 0.0, 0.0, Dimension::Overworld));
    let (_, d) = map.decorations().next().expect("marker kept");
    assert_eq!((d.kind.as_str(), d.x), ("player_off_limits", i8::MIN));
}

#[test]
fn a_marker_is_not_drawn_for_a_player_in_another_dimension() {
    let mut map = overworld_map();
    map.tick_carried_by(&carrier(Uuid::from_u128(1), 0.0, 0.0, 0.0, Dimension::End));
    assert_eq!(map.decorations().count(), 0);
}

#[test]
fn updates_carry_the_dirty_rectangle_and_resend_decorations_on_the_fifth_tick() {
    let mut map = overworld_map();
    let who = carrier(Uuid::from_u128(7), 0.0, 0.0, 0.0, Dimension::Overworld);
    map.tick_carried_by(&who);
    let first = map.next_update(who.uuid, 3).expect("first update");
    let patch = first.patch.expect("the whole grid goes out first");
    assert_eq!((patch.start_x, patch.start_y, patch.width, patch.height, patch.colors.len()), (0, 0, 128, 128, 16384));
    assert_eq!(first.decorations.expect("decorations ride the first update").len(), 1);
    assert!(map.next_update(who.uuid, 3).is_none());

    map.set_color(5, 9, 77);
    map.set_color(7, 10, 78);
    let update = map.next_update(who.uuid, 3).expect("dirty pixels");
    let patch = update.patch.expect("patch");
    assert_eq!((patch.start_x, patch.start_y, patch.width, patch.height), (5, 9, 3, 2));
    assert_eq!(patch.colors, vec![77, 0, 0, 0, 0, 78]);
    assert!(update.decorations.is_none());

    // A changed marker is dirty, but the list goes out only when the holder's counter reaches a
    // multiple of five: the counter advanced once per dirty check so far (twice), so the third
    // dirty check after this one is the fifth.
    let mut sends = 0;
    for step in 0..6 {
        map.tick_carried_by(&carrier(who.uuid, f64::from(step) + 1.0, 0.0, 0.0, Dimension::Overworld));
        if map.next_update(who.uuid, 3).is_some_and(|u| u.decorations.is_some()) {
            sends += 1;
        }
    }
    assert_eq!(sends, 1);
}

#[test]
fn banners_toggle_and_a_removed_banner_drops_its_marker() {
    let mut map = overworld_map();
    let pos = BlockPos::new(3, 64, 4);
    let red = banner_at(state("minecraft:red_banner[rotation=0]"), pos).expect("banner");
    assert_eq!(red.color, "red");
    assert!(map.toggle_banner(Some(red.clone()), pos));
    let (key, d) = map.decorations().next().expect("marker");
    // (3.5 * 2 + 0.5 = 7.5 -> 7, 4.5 * 2 + 0.5 = 9.5 -> 9, yaw 180 -> 8)
    assert_eq!((key, d.kind.as_str(), d.x, d.y, d.rotation), ("banner-3,64,4", "banner_red", 7, 9, 8));
    // Sampling the banner's column with the banner gone removes the marker.
    let who = carrier(Uuid::from_u128(1), 0.5, 0.5, 0.0, Dimension::Overworld);
    map.tick_carried_by(&who);
    map.sample_terrain(&who, &Grid(flat), false);
    assert!(map.decorations().all(|(key, _)| key != "banner-3,64,4"));
    assert!(map.banners.is_empty());
    // Off the map: refused.
    let far = BlockPos::new(500, 64, 0);
    assert!(!map.toggle_banner(banner_at(state("minecraft:red_banner[rotation=0]"), far), far));
    // Toggling an existing banner removes it.
    assert!(map.toggle_banner(Some(red.clone()), pos));
    assert!(map.toggle_banner(Some(red), pos));
    assert!(map.banners.is_empty());
}

fn scratch_directory(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("lodestone-maps-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

#[test]
fn maps_persist_in_the_reference_layout_and_load_back() {
    let directory = scratch_directory("persist");
    let handle = MapHandle::default();
    handle.attach_directory(directory.clone());
    let id = handle.create_for_use(200, -10, Dimension::Overworld);
    assert_eq!(id, 0);
    assert_eq!(handle.create_for_use(0, 0, Dimension::Nether), 1);
    handle.with(|store| {
        let map = store.get(0).unwrap();
        map.set_color(3, 4, 123);
        assert!(map.toggle_banner(banner_at(state("minecraft:blue_banner[rotation=4]"), BlockPos::new(250, 70, -2)), BlockPos::new(250, 70, -2)));
    });
    assert_eq!(handle.flush(), 3, "two maps and the counter");

    // The files and fields are the reference's.
    let root = lodestone_anvil::player_dat::read_from_file(&directory.join("0.dat")).unwrap().unwrap();
    let Some(Nbt::Compound(data)) = compound_get(&root, "data") else { panic!("missing data compound") };
    let names: Vec<_> = data.iter().map(|(name, _)| name.as_str()).collect();
    for expected in ["dimension", "xCenter", "zCenter", "scale", "colors", "trackingPosition", "unlimitedTracking", "locked", "banners", "frames"] {
        assert!(names.contains(&expected), "{expected} in {names:?}");
    }
    assert!(matches!(compound_get(&root, "DataVersion"), Some(Nbt::Int(_))));
    // (200 + 64) / 128 -> area 2 -> 2 * 128 + 64 - 64.
    assert_eq!(compound_get(&root, "data").and_then(|d| compound_get(d, "xCenter")), Some(&Nbt::Int(256)));
    let index = lodestone_anvil::player_dat::read_from_file(&directory.join("last_id.dat")).unwrap().unwrap();
    assert_eq!(compound_get(&index, "data").and_then(|d| compound_get(d, "map")), Some(&Nbt::Int(1)));

    // A fresh store reads them back, and the counter continues.
    let reopened = MapHandle::default();
    reopened.attach_directory(directory.clone());
    reopened.with(|store| {
        let map = store.get(0).expect("map 0 loads");
        assert_eq!((map.center_x, map.center_z, map.scale, map.color_at(3, 4)), (256, 0, 0, 123));
        assert_eq!(map.banners.len(), 1);
        assert!(map.decorations().any(|(key, d)| key == "banner-250,70,-2" && d.kind == "banner_blue"));
        assert_eq!(store.get(1).unwrap().dimension, "minecraft:the_nether");
    });
    assert_eq!(reopened.create_for_use(0, 0, Dimension::Overworld), 2);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_session_stream_starts_full_and_follows_the_hand() {
    let handle = MapHandle::default();
    let id = handle.create_for_use(0, 0, Dimension::Overworld);
    let mut inventory = PlayerInventory::new();
    let mut stack = ItemStack::new(ResourceKey::new("minecraft", "filled_map").unwrap(), 1);
    stack.components.map_id = Some(id);
    inventory.set_native(0, Some(stack.clone()));
    let mut session = MapSession::new(handle.clone(), Uuid::from_u128(9), "Mapper");
    let pose = PlayerPose { x: 0.5, z: 0.5, yaw: 0.0, dimension: Dimension::Overworld, game_time: 0 };

    struct Floor;
    impl ChunkSource for Floor {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            let mut column = ChunkColumn::new(-64, 384);
            for x in 0..16 {
                for z in 0..16 {
                    column.set_block_id(x, 64, z, state("minecraft:grass_block[snowy=false]"));
                }
            }
            column
        }
        fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
            self.column(0, 0).block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
        }
        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_owned()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
        fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
            Some(self.column(cx, cz))
        }
    }

    // Held in the selected hotbar slot: the full grid and the marker go out at once.
    let updates = session.tick(pose, &inventory, &Floor, &[]);
    let [first] = updates.as_slice() else { panic!("one update, got {updates:?}") };
    assert_eq!(first.map_id, id);
    let patch = first.patch.as_ref().unwrap();
    assert_eq!((patch.width, patch.height), (128, 128));
    assert_eq!(first.decorations.as_ref().map(Vec::len), Some(1));
    // Sampling began in the same tick: column 1 of the sent grid already holds grass.
    assert_eq!(patch.colors[1], GRASS_NORMAL);

    // Moved to a slot that is neither hand: still ticked and sent updates, no longer sampled.
    inventory.set_native(0, None);
    inventory.set_native(20, Some(stack));
    let colours_before = handle.with(|store| store.get(id).unwrap().colors.clone());
    let _ = session.tick(pose, &inventory, &Floor, &[]);
    assert_eq!(handle.with(|store| store.get(id).unwrap().colors.clone()), colours_before);

    // Dropping the session removes the marker.
    drop(session);
    assert_eq!(handle.with(|store| store.get(id).unwrap().decorations().count()), 0);
}

#[test]
fn an_unknown_map_id_is_ignored() {
    let handle = MapHandle::default();
    let mut inventory = PlayerInventory::new();
    let mut stack = ItemStack::new(ResourceKey::new("minecraft", "filled_map").unwrap(), 1);
    stack.components.map_id = Some(40);
    inventory.set_native(0, Some(stack));
    let mut session = MapSession::new(handle, Uuid::from_u128(9), "Mapper");
    let pose = PlayerPose { x: 0.0, z: 0.0, yaw: 0.0, dimension: Dimension::Overworld, game_time: 0 };
    struct Nothing;
    impl ChunkSource for Nothing {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(-64, 384)
        }
        fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
            StateId::AIR
        }
        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_owned()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
    }
    assert!(session.tick(pose, &inventory, &Nothing, &[]).is_empty());
}

fn framed(entity_id: i32, pos: BlockPos, rotation: i32) -> FramedMap {
    FramedMap { map_id: 0, entity_id, pos, rotation }
}

/// A frame's cell is read as integer block coordinates (no half-block centring), so the cell
/// (6, 3) on a map centred on the origin sits `2 * d + 0.5` truncated half-pixels out; an
/// east-facing frame turns 270 degrees, which is `(270 + 8) * 16 / 360` truncated sixteenths.
#[test]
fn a_frame_marker_sits_at_the_frames_cell_and_turns_with_its_facing() {
    let mut map = MapData::fresh(0.0, 0.0, 0, true, false, "minecraft:overworld");
    let who = carrier(Uuid::from_u128(1), 0.0, 0.0, 0.0, Dimension::Overworld);
    map.tick_in_frame(&who, &framed(7, BlockPos::new(6, 65, 3), 270), false);
    let markers: Vec<_> = map.decorations().map(|(key, d)| (key.to_owned(), d.kind.clone(), d.x, d.y, d.rotation)).collect();
    assert_eq!(markers, vec![("frame-7".to_owned(), "frame".to_owned(), 12, 6, 12)]);
    assert_eq!(map.frames, vec![MapFrame { pos: BlockPos::new(6, 65, 3), rotation: 270, entity_id: 7 }]);
    assert!(map.is_dirty());

    // A frame on a floor or ceiling turns -90 degrees: `(-90 - 8) * 16 / 360` truncates to -4, which is 12.
    map.tick_in_frame(&who, &framed(8, BlockPos::new(-6, 65, 0), -90), false);
    let rotation = map.decorations().find(|(key, _)| *key == "frame-8").map(|(_, d)| (d.x, d.y, d.rotation));
    // -6 * 2 + 0.5 = -11.5 truncates toward zero.
    assert_eq!(rotation, Some((-11, 0, 12)));

    // Another frame in the same cell replaces the first one's marker.
    map.tick_in_frame(&who, &framed(9, BlockPos::new(6, 65, 3), 270), false);
    let keys: Vec<_> = map.decorations().map(|(key, _)| key.to_owned()).collect();
    assert_eq!(keys, vec!["frame-8".to_owned(), "frame-9".to_owned()]);

    map.removed_from_frame(BlockPos::new(6, 65, 3), 9);
    map.removed_from_frame(BlockPos::new(-6, 65, 0), 8);
    assert_eq!(map.decorations().count(), 0);
    assert!(map.frames.is_empty());
}

#[test]
fn a_frame_off_the_map_shows_no_marker_and_a_frame_adds_no_holder_marker() {
    let mut map = MapData::fresh(0.0, 0.0, 0, true, false, "minecraft:overworld");
    let who = carrier(Uuid::from_u128(1), 0.0, 0.0, 0.0, Dimension::Overworld);
    map.tick_in_frame(&who, &framed(7, BlockPos::new(200, 65, 0), 0), false);
    assert_eq!(map.decorations().count(), 0, "63 blocks is the edge at scale 0");
    map.tick_in_frame(&who, &framed(7, BlockPos::new(1, 65, 0), 0), false);
    assert!(map.decorations().all(|(_, d)| d.kind == "frame"), "the viewer is not a holder");
}

#[test]
fn a_session_shows_a_framed_map_on_every_tenth_tick() {
    let handle = MapHandle::default();
    let id = handle.create_for_use(0, 0, Dimension::Overworld);
    let mut session = MapSession::new(handle.clone(), Uuid::from_u128(9), "Viewer");
    let pose = PlayerPose { x: 0.5, z: 0.5, yaw: 0.0, dimension: Dimension::Overworld, game_time: 0 };
    struct Nothing;
    impl ChunkSource for Nothing {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(-64, 384)
        }
        fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
            StateId::AIR
        }
        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_owned()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
    }
    let inventory = PlayerInventory::new();
    let frames = [FramedMap { map_id: id, entity_id: 4, pos: BlockPos::new(6, 65, 3), rotation: 270 }];
    for tick in 1..10 {
        assert!(session.tick(pose, &inventory, &Nothing, &frames).is_empty(), "tick {tick}");
    }
    let updates = session.tick(pose, &inventory, &Nothing, &frames);
    let [update] = updates.as_slice() else { panic!("one update, got {updates:?}") };
    assert_eq!(update.map_id, id);
    assert!(update.patch.is_some(), "the whole grid goes out with the first update");
    let kinds: Vec<_> = update.decorations.iter().flatten().map(|d| (d.kind.path().to_owned(), d.x, d.y, d.rotation)).collect();
    assert_eq!(kinds, vec![("frame".to_owned(), 12, 6, 12)]);

    // Once the frame is gone the viewer's holder is released on the next due tick.
    for _ in 0..10 {
        let _ = session.tick(pose, &inventory, &Nothing, &[]);
    }
    handle.removed_from_frame(id, BlockPos::new(6, 65, 3), 4);
    assert_eq!(handle.with(|store| store.get(id).unwrap().decorations().count()), 0);
}

#[test]
fn a_viewer_who_stopped_holding_the_map_loses_their_marker_but_a_holder_keeps_it() {
    let mut map = MapData::fresh(0.0, 0.0, 0, true, false, "minecraft:overworld");
    let who = carrier(Uuid::from_u128(1), 3.0, 3.0, 0.0, Dimension::Overworld);
    map.tick_carried_by(&who);
    assert_eq!(map.decorations().map(|(key, _)| key).collect::<Vec<_>>(), vec!["Mapper"]);
    map.tick_in_frame(&who, &framed(7, BlockPos::new(6, 65, 3), 270), true);
    assert_eq!(map.decorations().map(|(key, _)| key).collect::<Vec<_>>(), vec!["Mapper", "frame-7"]);
    map.tick_in_frame(&who, &framed(7, BlockPos::new(6, 65, 3), 270), false);
    assert_eq!(map.decorations().map(|(key, _)| key).collect::<Vec<_>>(), vec!["frame-7"]);
}
