//! Tests for the global animation-slot assignment: an [`Atlas`] stamps each
//! animated sprite with a dense slot id, and [`AnimTable`] resolves those ids
//! back into per-slot playback data. The baker copies a sprite's slot onto its
//! quads (see `tests/bake.rs`), so these two sides must agree by construction —
//! the property under test here is that the numbering is dense, stable, and
//! shared, with static sprites reserved to slot `0`.

use lodestone_assets::{
    AnimTable, AnimationMeta, AtlasBuilder, Image, ResourceLocation, TextureMeta,
};

/// A solid-colour RGBA8 image of the given size.
fn solid(width: u32, height: u32, color: [u8; 4]) -> Image {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        data.extend_from_slice(&color);
    }
    Image {
        width,
        height,
        rgba: data,
    }
}

/// A `frames`-tall vertical strip (each frame 16px) with an animation mcmeta.
fn animated(frames: u32, frametime: u32, interpolate: bool) -> (Image, TextureMeta) {
    let img = solid(16, 16 * frames, [10, 20, 30, 255]);
    let mut meta = TextureMeta::default();
    meta.animation = Some(AnimationMeta {
        frametime,
        interpolate,
        frame_width: None,
        frame_height: None,
        frames: Vec::new(),
    });
    (img, meta)
}

fn loc(s: &str) -> ResourceLocation {
    ResourceLocation::parse(s).unwrap()
}

#[test]
fn static_sprites_take_no_slot_and_animated_ones_are_numbered_densely() {
    let mut b = AtlasBuilder::new().with_width(64);
    // Interleave static and animated in non-sorted insert order to prove the
    // numbering follows the atlas's deterministic (location-sorted) order, not
    // insertion order.
    b.add_texture(loc("minecraft:block/stone"), solid(16, 16, [1, 1, 1, 255]), None);
    let (fire, fire_meta) = animated(4, 2, false);
    b.add_texture(loc("minecraft:block/fire_0"), fire, Some(fire_meta));
    b.add_texture(loc("minecraft:block/dirt"), solid(16, 16, [2, 2, 2, 255]), None);
    let (water, water_meta) = animated(2, 3, true);
    b.add_texture(loc("minecraft:block/water_still"), water, Some(water_meta));
    let atlas = b.build().expect("atlas builds");

    // Static sprites keep slot 0; animated sprites get 1.. in sorted order:
    // "fire_0" sorts before "water_still", so fire is slot 1, water is slot 2.
    let fire = atlas.sprite(&loc("minecraft:block/fire_0")).unwrap();
    let water = atlas.sprite(&loc("minecraft:block/water_still")).unwrap();
    let stone = atlas.sprite(&loc("minecraft:block/stone")).unwrap();
    assert_eq!(stone.anim_slot, 0, "static sprite stays static");
    assert_eq!(fire.anim_slot, 1);
    assert_eq!(water.anim_slot, 2);

    let table = AnimTable::from_atlas(&atlas);
    assert_eq!(table.len(), 2, "two animated sprites, two slots");
    // slots()[0] is slot id 1 (fire), slots()[1] is slot id 2 (water).
    assert_eq!(table.slots()[0].location, loc("minecraft:block/fire_0"));
    assert_eq!(table.slots()[1].location, loc("minecraft:block/water_still"));
}

#[test]
fn slot_frame_v_is_one_physical_frame_normalised() {
    let mut b = AtlasBuilder::new().with_width(16);
    let (fire, meta) = animated(4, 1, false);
    b.add_texture(loc("minecraft:block/fire_0"), fire, Some(meta));
    let atlas = b.build().expect("atlas builds");
    let table = AnimTable::from_atlas(&atlas);
    let slot = &table.slots()[0];

    // One frame is 16px tall; the offset to physical frame `n` is `n * frame_v`.
    let expected = 16.0 / atlas.height as f32;
    assert!(
        (slot.frame_v - expected).abs() < 1e-6,
        "frame_v {} != {expected}",
        slot.frame_v
    );
    assert_eq!(slot.frames.len(), 4, "natural order over four frames");
    for (n, f) in slot.frames.iter().enumerate() {
        assert_eq!(f.index, n as u32, "natural playback order");
        assert_eq!(f.hold_ticks, 1, "default frametime");
    }
    assert!(!slot.interpolate);
}

#[test]
fn interpolate_and_default_frametime_flow_through() {
    let mut b = AtlasBuilder::new().with_width(16);
    let (water, meta) = animated(2, 5, true);
    b.add_texture(loc("minecraft:block/water_still"), water, Some(meta));
    let atlas = b.build().expect("atlas builds");
    let table = AnimTable::from_atlas(&atlas);
    let slot = &table.slots()[0];
    assert!(slot.interpolate);
    assert!(slot.frames.iter().all(|f| f.hold_ticks == 5));
}
