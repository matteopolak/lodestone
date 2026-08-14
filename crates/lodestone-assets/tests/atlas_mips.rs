//! Tests for the atlas mip pyramid (per-sprite mip generation composited into
//! aligned atlas mip levels).

use lodestone_assets::{AtlasBuilder, Image, MipStrategy, ResourceLocation, generate_mip_levels};

fn solid(width: u32, height: u32, color: [u8; 4]) -> Image {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        rgba.extend_from_slice(&color);
    }
    Image {
        width,
        height,
        rgba,
    }
}

fn loc(s: &str) -> ResourceLocation {
    ResourceLocation::parse(s).unwrap()
}

#[test]
fn without_mips_there_is_only_the_base_level() {
    let mut b = AtlasBuilder::new();
    b.add_texture(
        loc("minecraft:block/a"),
        solid(16, 16, [10, 20, 30, 255]),
        None,
    );
    let atlas = b.build().unwrap();
    assert_eq!(atlas.mip_count(), 1);
    assert!(atlas.mip(1).is_none());
    let base = atlas.mip(0).unwrap();
    assert_eq!((base.width, base.height), (atlas.width, atlas.height));
    assert_eq!(base.rgba, atlas.rgba.as_slice());
}

#[test]
fn requesting_mip_levels_produces_a_halving_pyramid() {
    let mut b = AtlasBuilder::new().with_mip_levels(3);
    b.add_texture(
        loc("minecraft:block/a"),
        solid(16, 16, [10, 20, 30, 255]),
        None,
    );
    b.add_texture(
        loc("minecraft:block/b"),
        solid(16, 16, [200, 100, 50, 255]),
        None,
    );
    let atlas = b.build().unwrap();
    assert_eq!(atlas.mip_count(), 4);
    for level in 0..4u32 {
        let m = atlas.mip(level).unwrap();
        assert_eq!(
            (m.width, m.height),
            (atlas.width >> level, atlas.height >> level)
        );
        assert_eq!(m.rgba.len(), (m.width * m.height * 4) as usize);
    }
    assert!(atlas.mip(4).is_none());
}

#[test]
fn each_sprites_mip_region_holds_its_own_downsample_not_a_neighbours() {
    // Two solid sprites of very different colours. At mip level 1, the texel at
    // each sprite's reduced origin must be that sprite's colour (a solid colour
    // mips to itself), proving per-sprite mips are placed with no cross-bleed.
    let mut b = AtlasBuilder::new().with_mip_levels(2);
    let red = [220, 20, 20, 255];
    let blue = [20, 20, 220, 255];
    b.add_texture(loc("minecraft:block/red"), solid(16, 16, red), None);
    b.add_texture(loc("minecraft:block/blue"), solid(16, 16, blue), None);
    let atlas = b.build().unwrap();

    for (name, color) in [("minecraft:block/red", red), ("minecraft:block/blue", blue)] {
        let s = atlas.sprite(&loc(name)).unwrap();
        // Expected solid-colour mip value (linear round-trip through the LUT).
        let expect =
            generate_mip_levels(&solid(16, 16, color), 1, MipStrategy::Auto, 0.0)[1].pixel(0, 0);
        let m = atlas.mip(1).unwrap();
        let (mx, my) = (s.x >> 1, s.y >> 1);
        let i = ((my * m.width + mx) * 4) as usize;
        let got = [m.rgba[i], m.rgba[i + 1], m.rgba[i + 2], m.rgba[i + 3]];
        assert_eq!(
            got, expect,
            "sprite {name} mip level 1 origin should be its own downsample"
        );
    }
}

#[test]
fn effective_levels_are_capped_by_the_smallest_sprite() {
    // An 8x8 sprite supports only 3 levels; even if 4 are requested the pyramid
    // stops at what every sprite can supply.
    let mut b = AtlasBuilder::new().with_mip_levels(4);
    b.add_texture(
        loc("minecraft:block/big"),
        solid(16, 16, [1, 2, 3, 255]),
        None,
    );
    b.add_texture(
        loc("minecraft:block/small"),
        solid(8, 8, [4, 5, 6, 255]),
        None,
    );
    let atlas = b.build().unwrap();
    assert_eq!(
        atlas.mip_count(),
        4,
        "min(4 requested, 3 from the 8x8 sprite) => levels 0..=3"
    );
}

/// Sprites bleed into each other at distance: a GPU sampler
/// minifying with `Linear` reads across a sprite boundary unless the atlas
/// reserves a gutter, and that gutter has to be re-extruded (filled from the
/// sprite's own edge) at *every* mip level, not just level 0 — `pad` itself
/// halves with the level, and a level whose gutter is left at the backing
/// buffer's zero-init value blends a sprite's edge toward transparent black
/// instead of toward its own colour, which is the same visible seam a missing
/// gutter produces. This checks a *deep* level (2, not just the base) so it
/// cannot pass by accident of level-0-only extrusion.
#[test]
fn padded_mip_gutter_extrudes_the_sprites_own_edge_at_every_level() {
    let mut b = AtlasBuilder::new().with_mip_levels(4).with_padding(16);
    let red = [220, 20, 20, 255];
    let blue = [20, 20, 220, 255];
    b.add_texture(loc("minecraft:block/red"), solid(16, 16, red), None);
    b.add_texture(loc("minecraft:block/blue"), solid(16, 16, blue), None);
    let atlas = b.build().unwrap();

    let level = 2;
    let m = atlas.mip(level).unwrap();
    for (name, color) in [("minecraft:block/red", red), ("minecraft:block/blue", blue)] {
        let s = atlas.sprite(&loc(name)).unwrap();
        let expect =
            generate_mip_levels(&solid(16, 16, color), level, MipStrategy::Auto, 0.0)[level as usize]
                .pixel(0, 0);
        let (sx, sy) = (s.x >> level, s.y >> level);
        let sw = s.width >> level;
        // One texel past this sprite's own right edge at this level: exactly
        // where a bilinear sample taken at the sprite's UV edge lands its
        // second tap.
        let outside_x = sx + sw;
        let mid_y = sy + (s.height >> level) / 2;
        let i = ((mid_y * m.width + outside_x) * 4) as usize;
        let got = [m.rgba[i], m.rgba[i + 1], m.rgba[i + 2], m.rgba[i + 3]];
        assert_eq!(
            got, expect,
            "sprite {name}'s gutter one texel past its own right edge at mip level {level} must \
             be its own extruded colour, not zero-filled (transparent black) or the neighbour's"
        );
    }
}

/// The negative-control companion to the test above: with no padding
/// requested (the historical, pre-fix production configuration), the
/// identical probe position must read as the *neighbouring* sprite's colour —
/// proving the assertion above is actually sensitive to the defect it guards,
/// not merely satisfied by coincidence.
#[test]
fn unpadded_atlas_bleeds_the_neighbour_into_the_same_probe_position() {
    let mut b = AtlasBuilder::new().with_mip_levels(4);
    let red = [220, 20, 20, 255];
    let blue = [20, 20, 220, 255];
    b.add_texture(loc("minecraft:block/red"), solid(16, 16, red), None);
    b.add_texture(loc("minecraft:block/blue"), solid(16, 16, blue), None);
    let atlas = b.build().unwrap();

    let level = 2;
    let m = atlas.mip(level).unwrap();
    let s = atlas.sprite(&loc("minecraft:block/red")).unwrap();
    let (sx, sy) = (s.x >> level, s.y >> level);
    let sw = s.width >> level;
    let outside_x = sx + sw;
    let mid_y = sy + (s.height >> level) / 2;
    let i = ((mid_y * m.width + outside_x) * 4) as usize;
    let got = [m.rgba[i], m.rgba[i + 1], m.rgba[i + 2], m.rgba[i + 3]];
    let expect_blue =
        generate_mip_levels(&solid(16, 16, blue), level, MipStrategy::Auto, 0.0)[level as usize]
            .pixel(0, 0);
    assert_eq!(
        got, expect_blue,
        "without padding, the red sprite's right-edge gutter at mip level {level} must read as \
         the blue neighbour's own colour -- this is issue #575's bleed, reproduced here as the \
         control proving the assertion in the padded test above means something"
    );
}

#[test]
fn mip_pyramid_is_deterministic() {
    let build = || {
        let mut b = AtlasBuilder::new().with_mip_levels(3);
        b.add_texture(
            loc("minecraft:block/a"),
            solid(16, 16, [10, 20, 30, 255]),
            None,
        );
        b.add_texture(
            loc("minecraft:block/b"),
            solid(32, 16, [200, 100, 50, 255]),
            None,
        );
        b.build().unwrap()
    };
    let a = build();
    let c = build();
    for level in 0..a.mip_count() {
        assert_eq!(a.mip(level).unwrap().rgba, c.mip(level).unwrap().rgba);
    }
}
