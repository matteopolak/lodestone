//! Hermetic tests for the flat item-sprite atlas.
//!
//! The [`ItemAtlas`] stitches every `item/generated` sprite the item corpus
//! resolves to into one texture the renderer can sample, and caches each item's
//! resolved [`ItemIcon`] so the draw path never re-resolves per frame. Fixtures
//! are in-memory packs, so nothing here needs `client.jar`.

use lodestone_assets::{IconPart, ItemAtlas, MemorySource, ResourceLocation, ResourceManager};

fn loc(s: &str) -> ResourceLocation {
    ResourceLocation::parse(s).unwrap()
}

/// A solid `w`x`h` RGBA PNG of one colour.
fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&rgba);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(&buf).unwrap();
    }
    out
}

/// A pack with two generated items (diamond, apple), a block item (stone), and
/// the item/generated + block parents. Textures are 16x16 solids.
fn manager() -> ResourceManager {
    let mut src = MemorySource::new("test");
    let mut ins = |path: &str, body: Vec<u8>| src.insert(path.to_string(), body);

    // Two flat generated items.
    ins(
        "assets/minecraft/items/diamond.json",
        br#"{"model":{"type":"minecraft:model","model":"minecraft:item/diamond"}}"#.to_vec(),
    );
    ins(
        "assets/minecraft/models/item/diamond.json",
        br#"{"parent":"minecraft:item/generated","textures":{"layer0":"minecraft:item/diamond"}}"#.to_vec(),
    );
    ins("assets/minecraft/textures/item/diamond.png", png(16, 16, [0, 200, 255, 255]));

    ins(
        "assets/minecraft/items/apple.json",
        br#"{"model":{"type":"minecraft:model","model":"minecraft:item/apple"}}"#.to_vec(),
    );
    ins(
        "assets/minecraft/models/item/apple.json",
        br#"{"parent":"minecraft:item/generated","textures":{"layer0":"minecraft:item/apple"}}"#.to_vec(),
    );
    ins("assets/minecraft/textures/item/apple.png", png(16, 16, [220, 40, 40, 255]));

    // A block item -> a Model part, not a flat sprite. Its block texture must
    // NOT be required by the flat atlas.
    ins(
        "assets/minecraft/items/stone.json",
        br#"{"model":{"type":"minecraft:model","model":"minecraft:block/stone"}}"#.to_vec(),
    );
    ins(
        "assets/minecraft/models/block/stone.json",
        br#"{"parent":"minecraft:block/cube_all","textures":{"all":"minecraft:block/stone"}}"#.to_vec(),
    );
    ins(
        "assets/minecraft/models/block/cube_all.json",
        br##"{"parent":"minecraft:block/cube","textures":{"particle":"#all","down":"#all","up":"#all","north":"#all","east":"#all","south":"#all","west":"#all"}}"##.to_vec(),
    );
    ins(
        "assets/minecraft/models/block/cube.json",
        br##"{"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"down":{"texture":"#down"},"up":{"texture":"#up"},"north":{"texture":"#north"},"south":{"texture":"#south"},"west":{"texture":"#west"},"east":{"texture":"#east"}}}]}"##.to_vec(),
    );

    // item/generated parent chain.
    ins(
        "assets/minecraft/models/item/generated.json",
        br#"{"parent":"minecraft:builtin/generated"}"#.to_vec(),
    );

    ResourceManager::new(vec![Box::new(src)])
}

#[test]
fn stitches_flat_item_sprites_with_uvs() {
    let mgr = manager();
    let atlas = ItemAtlas::build(&mgr).expect("build item atlas");

    // Both flat items resolved and cached.
    let diamond = atlas.icon(&loc("minecraft:diamond")).expect("diamond icon");
    match &diamond.parts[0] {
        IconPart::Sprite { layers } => {
            assert_eq!(layers[0].sprite, loc("minecraft:item/diamond"));
        }
        other => panic!("expected Sprite, got {other:?}"),
    }

    // The sprite is present in the stitched atlas with a non-degenerate UV rect.
    let sprite = atlas
        .sprite(&loc("minecraft:item/diamond"))
        .expect("diamond sprite in atlas");
    assert!(sprite.uv_max[0] > sprite.uv_min[0]);
    assert!(sprite.uv_max[1] > sprite.uv_min[1]);
    assert!(atlas.sprite(&loc("minecraft:item/apple")).is_some());
}

#[test]
fn block_item_is_a_model_and_not_in_flat_atlas() {
    let mgr = manager();
    let atlas = ItemAtlas::build(&mgr).expect("build item atlas");

    let stone = atlas.icon(&loc("minecraft:stone")).expect("stone icon");
    assert!(
        matches!(stone.parts[0], IconPart::Model { .. }),
        "block item must resolve to a Model part, got {:?}",
        stone.parts[0]
    );
    // The flat atlas holds item sprites only; the block face lives in the block
    // atlas, so it is absent here (the 3-D path samples that atlas instead).
    assert!(atlas.sprite(&loc("minecraft:block/stone")).is_none());
}

#[test]
fn report_counts_drawable_items_and_names_missing_textures() {
    let mut src = MemorySource::new("test");
    // A generated item whose texture is missing: it must be reported, not fatal.
    src.insert(
        "assets/minecraft/items/ghost.json".to_string(),
        br#"{"model":{"type":"minecraft:model","model":"minecraft:item/ghost"}}"#.to_vec(),
    );
    src.insert(
        "assets/minecraft/models/item/ghost.json".to_string(),
        br#"{"parent":"minecraft:item/generated","textures":{"layer0":"minecraft:item/ghost"}}"#.to_vec(),
    );
    src.insert(
        "assets/minecraft/models/item/generated.json".to_string(),
        br#"{"parent":"minecraft:builtin/generated"}"#.to_vec(),
    );
    let mgr = ResourceManager::new(vec![Box::new(src)]);

    let (atlas, report) = ItemAtlas::build_reported(&mgr).expect("build");
    assert_eq!(report.items, 1);
    assert!(
        report.missing_textures.iter().any(|m| m.contains("ghost")),
        "missing texture should be named, got {:?}",
        report.missing_textures
    );
    // The ghost icon still resolves (definition is valid); only its pixels are absent.
    assert!(atlas.icon(&loc("minecraft:ghost")).is_some());
}
