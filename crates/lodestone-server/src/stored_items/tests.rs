use super::*;

fn stack(name: &str, count: u32) -> ItemStack {
    ItemStack::new(name.parse().expect("valid key"), count)
}

fn reference_root() -> Option<std::path::PathBuf> {
    let root = lodestone_mc_cache::version_root(&lodestone_mc_cache::current_version()).join("src/data/minecraft");
    root.is_dir().then_some(root)
}

fn tag_values(root: &std::path::Path, tag: &str) -> Vec<String> {
    let text = std::fs::read_to_string(root.join(format!("tags/item/{tag}.json"))).expect("reference tag");
    let json: serde_json::Value = serde_json::from_str(&text).expect("tag json");
    json["values"].as_array().expect("values").iter().map(|v| v.as_str().expect("string").to_owned()).collect()
}

/// Hit positions worked out by hand from the 3 x 2 grid: columns split the
/// width in thirds, the top half of the face is row 0.
#[test]
fn the_hit_position_selects_the_slot() {
    let cases = [
        // north: the viewer's left is +x, so across = 1 - x.
        (Direction::North, (0.9, 0.75, 0.0), Some(0)),
        (Direction::North, (0.1, 0.2, 0.0), Some(5)),
        (Direction::North, (0.5, 0.9, 0.0), Some(1)),
        // south: across = x.
        (Direction::South, (0.5, 0.5, 1.0), Some(4)),
        (Direction::South, (0.9, 0.6, 1.0), Some(2)),
        (Direction::South, (0.1, 0.1, 1.0), Some(3)),
        // west: across = z; east: across = 1 - z.
        (Direction::West, (0.0, 0.9, 0.2), Some(0)),
        (Direction::East, (1.0, 0.9, 0.2), Some(2)),
        (Direction::East, (1.0, 0.1, 0.9), Some(3)),
    ];
    for (facing, cursor, expected) in cases {
        assert_eq!(hit_slot(facing, facing, cursor), expected, "{facing:?} {cursor:?}");
    }
    // Any other face, or the top, selects nothing.
    assert_eq!(hit_slot(Direction::North, Direction::South, (0.5, 0.5, 0.5)), None);
    assert_eq!(hit_slot(Direction::North, Direction::Up, (0.5, 1.0, 0.5)), None);
}

#[test]
fn the_disc_table_matches_the_reference_songs() {
    let Some(root) = reference_root() else { return eprintln!("SKIP: no reference tree") };
    let mut songs: Vec<(String, u8)> = std::fs::read_dir(root.join("jukebox_song"))
        .expect("song directory")
        .map(|entry| {
            let path = entry.expect("entry").path();
            let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("song")).expect("json");
            let name = path.file_stem().expect("stem").to_string_lossy().into_owned();
            (name, json["comparator_output"].as_u64().expect("output") as u8)
        })
        .collect();
    // The synced registry orders entries alphabetically by key.
    songs.sort();
    let table: Vec<(String, u8)> =
        crate::container_signal::DISC_SIGNALS.iter().map(|&(name, signal)| (name.to_owned(), signal)).collect();
    assert_eq!(table, songs);
    let cat = songs.iter().position(|(name, _)| name == "cat").expect("cat");
    assert_eq!(song_of(&stack("minecraft:music_disc_cat", 1)), Some((cat as i32, 2)));
    assert_eq!(song_of(&stack("minecraft:stick", 1)), None);
}

#[test]
fn the_book_and_breaking_tool_sets_match_the_reference_tags() {
    let Some(root) = reference_root() else { return eprintln!("SKIP: no reference tree") };
    let books = tag_values(&root, "bookshelf_books");
    assert_eq!(books.len(), BOOKSHELF_BOOKS.len());
    assert!(books.iter().all(|book| is_shelvable_book(book)));
    assert!(!is_shelvable_book("minecraft:paper"));

    let mut breakers = Vec::new();
    for tag in tag_values(&root, "breaks_decorated_pots") {
        match tag.strip_prefix('#') {
            Some(inner) => breakers.extend(tag_values(&root, inner.strip_prefix("minecraft:").expect("namespace"))),
            None => breakers.push(tag),
        }
    }
    assert!(breakers.len() > 30, "swords, axes, pickaxes, shovels, hoes, trident, mace");
    for item in &breakers {
        assert!(tool_shatters_pot(Some(&stack(item, 1))), "{item}");
    }
    for item in tag_values(&root, "spears") {
        assert!(!tool_shatters_pot(Some(&stack(&item, 1))), "{item} is not in the tag");
    }
    assert!(!tool_shatters_pot(Some(&stack("minecraft:stick", 1))));
    assert!(!tool_shatters_pot(None));
    let mut silk = stack("minecraft:diamond_pickaxe", 1);
    silk.components.enchantments.push(lodestone_model::ItemEnchantment {
        id: crate::enchantment_data::id_of("minecraft:silk_touch").expect("silk touch"),
        level: 1,
    });
    assert!(!tool_shatters_pot(Some(&silk)), "silk touch keeps the pot whole");
}

#[test]
fn a_pot_takes_one_kind_up_to_the_stack_limit() {
    // Ender pearls stack to sixteen.
    let pearls = stack("minecraft:ender_pearl", 40);
    let (first, fill) = pot_insert(None, &pearls).expect("empty pot accepts");
    assert_eq!((first.count, fill), (1, 1.0 / 16.0));
    let mut current = first;
    for expected in 2..=16 {
        let (next, fill) = pot_insert(Some(&current), &pearls).expect("room left");
        assert_eq!(next.count, expected);
        assert_eq!(fill, expected as f32 / 16.0);
        current = next;
    }
    assert!(pot_insert(Some(&current), &pearls).is_none(), "full at sixteen");
    assert!(pot_insert(Some(&stack("minecraft:stick", 3)), &pearls).is_none(), "another kind is refused");
    let mut named = pearls.clone();
    named.components.custom_name = Some(lodestone_model::Text::literal("x"));
    assert!(pot_insert(Some(&stack("minecraft:ender_pearl", 2)), &named).is_none(), "components must match");
}

#[test]
fn bookshelf_flags_follow_the_slots() {
    let state = StateId::from_state_str("minecraft:chiseled_bookshelf[facing=north,slot_0_occupied=false,slot_1_occupied=false,slot_2_occupied=false,slot_3_occupied=false,slot_4_occupied=false,slot_5_occupied=false]").expect("state");
    let mut items = vec![None; BOOKSHELF_SLOTS];
    items[1] = Some(stack("minecraft:book", 1));
    items[5] = Some(stack("minecraft:book", 1));
    let next = bookshelf_state(state, &items).expect("state exists");
    let flags: Vec<&str> = next.properties().iter().filter(|(k, _)| k.starts_with("slot_")).map(|&(_, v)| v).collect();
    assert_eq!(flags, ["false", "true", "false", "false", "false", "true"]);
    assert_eq!(next.properties().iter().find(|(k, _)| *k == "facing").map(|&(_, v)| v), Some("north"));

    let mut nbt = fresh(BOOKSHELF_ID, BlockPos::new(1, 2, 3), None).and_then(|e| match e {
        BlockEntity::Opaque { nbt, .. } => Some(nbt),
        _ => None,
    }).expect("fresh shelf");
    set_bookshelf_slot(&mut nbt, 4, Some(stack("minecraft:book", 1)));
    assert_eq!(field(&nbt, "last_interacted_slot"), Some(&Nbt::Int(4)));
    assert_eq!(bookshelf_items(&nbt)[4].as_ref().map(|s| s.item.to_string()), Some("minecraft:book".to_owned()));
    assert_eq!(crate::container_signal::entity_signal(&BlockEntity::Opaque { id: BOOKSHELF_ID.to_owned().into(), nbt }), Some(5));
}

#[test]
fn a_pot_placed_from_a_decorated_item_keeps_its_sherds() {
    let mut item = stack("minecraft:decorated_pot", 1);
    item.components.pot_decorations = Some(PotDecorations {
        back: Some("minecraft:angler_pottery_sherd".parse().expect("key")),
        left: None,
        right: Some("minecraft:arms_up_pottery_sherd".parse().expect("key")),
        front: None,
    });
    let Some(BlockEntity::Opaque { nbt, .. }) = fresh(POT_ID, BlockPos::new(0, 0, 0), Some(&item)) else {
        panic!("a pot entity");
    };
    assert_eq!(
        sherds(&nbt),
        ["minecraft:angler_pottery_sherd", "minecraft:brick", "minecraft:arms_up_pottery_sherd", "minecraft:brick"]
    );
    assert!(fresh("minecraft:chest", BlockPos::new(0, 0, 0), None).is_none());
}

#[test]
fn broken_blocks_give_up_what_they_hold() {
    let shulker = StateId::from_state_str("minecraft:red_shulker_box[facing=up]").expect("state");
    let mut slots = vec![None; 27];
    slots[0] = Some(stack("minecraft:diamond", 5));
    slots[26] = Some(stack("minecraft:stick", 7));
    let container = BlockEntity::Container { id: crate::block_entities::SHULKER_BOX_ID.to_owned().into(), slots: slots.clone() };
    let drops = broken_drops(shulker, Some(&container), true);
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].item.to_string(), "minecraft:red_shulker_box");
    assert_eq!(drops[0].components.container, slots);
    assert!(broken_drops(shulker, Some(&container), false).is_empty(), "the item follows the drop rule");

    let pot_whole = StateId::from_state_str("minecraft:decorated_pot[cracked=false,facing=north,waterlogged=false]").expect("state");
    let pot_cracked = StateId::from_state_str("minecraft:decorated_pot[cracked=true,facing=north,waterlogged=false]").expect("state");
    let Some(BlockEntity::Opaque { id, mut nbt }) = fresh(POT_ID, BlockPos::new(0, 0, 0), None) else { panic!("pot") };
    set_pot_item(&mut nbt, Some(&stack("minecraft:stick", 3)));
    let pot = BlockEntity::Opaque { id, nbt };
    let whole = broken_drops(pot_whole, Some(&pot), true);
    let names: Vec<String> = whole.iter().map(|s| s.item.to_string()).collect();
    assert_eq!(names, ["minecraft:decorated_pot", "minecraft:stick"]);
    assert_eq!(whole[1].count, 3);
    let cracked = broken_drops(pot_cracked, Some(&pot), true);
    let names: Vec<String> = cracked.iter().map(|s| s.item.to_string()).collect();
    assert_eq!(names, ["minecraft:brick", "minecraft:brick", "minecraft:brick", "minecraft:brick", "minecraft:stick"]);
}

/// A shulker box item's contents survive the save form, empty slots included.
#[test]
fn a_shulker_items_contents_round_trip_through_nbt() {
    let mut inner = vec![None; 27];
    inner[0] = Some(stack("minecraft:cobblestone", 64));
    inner[13] = Some(stack("minecraft:diamond", 3));
    inner[26] = Some(stack("minecraft:stick", 7));
    let mut shulker = stack("minecraft:shulker_box", 1);
    shulker.components.container = inner.clone();
    let nbt = crate::chunk_nbt::items_to_nbt(&[Some(shulker)]);
    let back = crate::chunk_nbt::items_from_nbt(Some(&nbt), 1);
    let kept = &back[0].as_ref().expect("the box survives").components.container;
    let occupied: Vec<(usize, String, u32)> = kept
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.as_ref().map(|s| (i, s.item.to_string(), s.count)))
        .collect();
    assert_eq!(
        occupied,
        [(0, "minecraft:cobblestone".to_owned(), 64), (13, "minecraft:diamond".to_owned(), 3), (26, "minecraft:stick".to_owned(), 7)]
    );
}

fn playing_jukebox(disc: &str, ticks: i64) -> Nbt {
    let mut nbt = base(JUKEBOX_ID, BlockPos::new(0, 64, 0));
    set_jukebox_disc(&mut nbt, Some(&stack(disc, 1)));
    set_field(&mut nbt, "ticks_since_song_started", Nbt::Long(ticks));
    nbt
}

/// Every disc's song length comes from the reference song file: the length in
/// seconds rounded up to ticks, plus one second of padding.
#[test]
fn song_lengths_are_the_reference_ones() {
    let Some(root) = reference_root() else {
        eprintln!("reference cache absent; skipping");
        return;
    };
    let mut checked = 0;
    for (index, (name, _)) in crate::container_signal::DISC_SIGNALS.iter().enumerate() {
        let text = std::fs::read_to_string(root.join(format!("jukebox_song/{name}.json"))).expect("song file");
        let json: serde_json::Value = serde_json::from_str(&text).expect("song json");
        let seconds = json["length_in_seconds"].as_f64().expect("length");
        let expected = (seconds * 20.0).ceil() as u64 + 20;
        assert_eq!(song_ticks(&stack(&format!("minecraft:music_disc_{name}"), 1)), Some(expected), "{name}");
        assert_eq!(song_of(&stack(&format!("minecraft:music_disc_{name}"), 1)).map(|(id, _)| id as usize), Some(index));
        checked += 1;
    }
    assert_eq!(checked, 22);
}

/// "11" is 71 seconds: 1420 ticks plus 20 of padding. It plays for exactly
/// 1440 ticks and ends on the next, leaving the disc and clearing the clock.
#[test]
fn a_song_ends_on_the_tick_after_its_length() {
    let mut nbt = playing_jukebox("minecraft:music_disc_11", 0);
    for tick in 0..1440 {
        assert!(!tick_jukebox(&mut nbt), "still playing at tick {tick}");
        assert!(jukebox_is_playing(&nbt));
    }
    assert!(tick_jukebox(&mut nbt), "the song ends after 1440 ticks");
    assert!(!jukebox_is_playing(&nbt));
    assert!(jukebox_disc(&nbt).is_some(), "the disc stays in the jukebox");
    assert!(!tick_jukebox(&mut nbt), "an idle jukebox reports nothing");
}

/// Control: a jukebox that was never started does not tick, and one holding a
/// disc with no song ends at once rather than playing forever.
#[test]
fn an_idle_jukebox_stays_idle() {
    let mut idle = base(JUKEBOX_ID, BlockPos::new(0, 64, 0));
    set_jukebox_disc(&mut idle, Some(&stack("minecraft:music_disc_11", 1)));
    set_field(&mut idle, "ticks_since_song_started", Nbt::Long(5));
    remove_field(&mut idle, "ticks_since_song_started");
    assert!(!tick_jukebox(&mut idle));
    let mut stray = playing_jukebox("minecraft:stone", 0);
    assert!(tick_jukebox(&mut stray));
}

/// A dropped shulker box carries its slots up to the last occupied one:
/// trailing empty slots are trimmed, and an empty box carries nothing. Other
/// containers spill every stack and drop only their own item.
#[test]
fn a_dropped_box_trims_trailing_empty_slots_and_chests_spill() {
    let shulker = StateId::from_state_str("minecraft:shulker_box[facing=up]").expect("state");
    let mut slots = vec![None; 27];
    slots[1] = Some(stack("minecraft:diamond", 5));
    let boxed = BlockEntity::Container { id: crate::block_entities::SHULKER_BOX_ID.to_owned().into(), slots };
    let drops = broken_drops(shulker, Some(&boxed), true);
    assert_eq!(drops[0].components.container.len(), 2, "slots 0 and 1 travel, the 25 empty ones after do not");
    let empty = BlockEntity::container(crate::block_entities::SHULKER_BOX_ID);
    assert!(broken_drops(shulker, Some(&empty), true)[0].components.container.is_empty());

    let chest = StateId::from_state_str("minecraft:chest[facing=north,type=single,waterlogged=false]").expect("state");
    let mut held = BlockEntity::container("minecraft:chest");
    if let BlockEntity::Container { slots, .. } = &mut held {
        slots[3] = Some(stack("minecraft:apple", 2));
        slots[20] = Some(stack("minecraft:stick", 9));
    }
    let spilled: Vec<(String, u32)> = broken_drops(chest, Some(&held), false).iter().map(|s| (s.item.to_string(), s.count)).collect();
    assert_eq!(spilled, [("minecraft:apple".to_owned(), 2), ("minecraft:stick".to_owned(), 9)], "a chest spills every stack, even with drops off");
}
