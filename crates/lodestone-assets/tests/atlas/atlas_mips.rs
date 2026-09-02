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

/// The `*.png.mcmeta` `texture` section reaches per-sprite mip generation.
///
/// This is a wiring gate, not an algorithm one: `mipmap.rs` has implemented all
/// five of vanilla's strategies since it was written, and for a long time
/// nothing selected any of them — `AtlasBuilder::build` passed `MipStrategy::Auto`
/// and a `0.0` bias unconditionally, so `StrictCutout`, `DarkCutout` and an
/// explicit `Mean` had no producer at all. In the real 26.2 jar 45 block sprites
/// ask for one of those (every leaves texture wants `dark_cutout`, 27 flower and
/// amethyst sprites want `strict_cutout`, glass and the redstone-dust sprites
/// want `mean`, and cactus/kelp/tripwire carry a `0.1` cutoff bias), and those are
/// exactly the sprites whose alpha the terrain shader thresholds.
///
/// The subject is a half-opaque, half-fully-transparent sprite — an image on
/// which the strategies genuinely disagree — and the assertions are *pairwise
/// difference* against the no-mcmeta build, plus a same-build control proving
/// the comparison is not simply reporting that every atlas differs.
#[test]
fn a_sprites_mcmeta_mipmap_strategy_selects_its_downsample() {
    use lodestone_assets::TextureMeta;

    // Left half opaque red, right half fully transparent: `solidify` (Cutout),
    // `fill_empty_with_dark` (DarkCutout) and a plain linear mean all produce
    // different bytes here, and a 0.3-vs-0.5 coverage reference lands the
    // alpha rescale differently again.
    fn half_cutout() -> Image {
        let mut rgba = Vec::with_capacity(16 * 16 * 4);
        for _ in 0..16 {
            for x in 0..16 {
                if x < 8 {
                    rgba.extend_from_slice(&[220, 40, 40, 255]);
                } else {
                    rgba.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        Image {
            width: 16,
            height: 16,
            rgba,
        }
    }

    fn level1(meta: Option<TextureMeta>) -> Vec<u8> {
        let mut b = AtlasBuilder::new().with_mip_levels(4).with_padding(16);
        b.add_texture(loc("minecraft:block/subject"), half_cutout(), meta);
        let atlas = b.build().unwrap();
        atlas.mip(1).unwrap().rgba.to_vec()
    }

    fn meta(json: &str) -> Option<TextureMeta> {
        Some(TextureMeta::parse(json.as_bytes()).expect("mcmeta parses"))
    }

    let default = level1(None);

    // The control: a second build with no mcmeta must be byte-identical, so a
    // difference below is the strategy and not build nondeterminism.
    assert_eq!(
        default,
        level1(None),
        "two no-mcmeta builds of the same sprite must be byte-identical"
    );
    // ... and an explicit `auto` must agree with no mcmeta at all, which is
    // what makes `auto` the right default rather than merely the current one.
    assert_eq!(
        default,
        level1(meta(r#"{"texture":{"mipmap_strategy":"auto"}}"#)),
        "an explicit \"auto\" must produce exactly the no-mcmeta chain"
    );

    // Collect rather than assert inside the loop: an `assert_ne!` in the body
    // aborts on the first unwired arm, so a neuter would only ever demonstrate
    // one of them and the rest would stay arguments instead of observations.
    // The bias gets its own row because it is a separate argument on the same
    // call — a strategy can be threaded through while the bias stays hardcoded.
    let mut unwired: Vec<&str> = Vec::new();
    for (label, json) in [
        ("mean", r#"{"texture":{"mipmap_strategy":"mean"}}"#),
        (
            "strict_cutout",
            r#"{"texture":{"mipmap_strategy":"strict_cutout"}}"#,
        ),
        (
            "dark_cutout",
            r#"{"texture":{"mipmap_strategy":"dark_cutout"}}"#,
        ),
        ("alpha_cutoff_bias", r#"{"texture":{"alpha_cutoff_bias":0.1}}"#),
    ] {
        if level1(meta(json)) == default {
            unwired.push(label);
        }
    }
    assert!(
        unwired.is_empty(),
        "these mcmeta inputs never reached mip generation (byte-identical to the no-mcmeta \
         chain), which means AtlasBuilder is still passing MipStrategy::Auto / 0.0 regardless \
         of the sprite's own metadata: {unwired:?}"
    );
}

/// A sprite with a transparent hole, in a checkerboard so the hole's *nearest
/// opaque neighbour* is unambiguous: the hole texel at `(1, 1)` is surrounded
/// on all four sides by one colour and diagonally by another, and `solidify`'s
/// four-neighbour BFS therefore has exactly one answer.
fn holed(color: [u8; 4], hole_rgb: [u8; 3]) -> Image {
    let mut img = solid(16, 16, color);
    let i = ((1 * 16 + 1) * 4) as usize;
    img.rgba[i] = hole_rgb[0];
    img.rgba[i + 1] = hole_rgb[1];
    img.rgba[i + 2] = hole_rgb[2];
    img.rgba[i + 3] = 0;
    img
}

fn texel_at(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

/// Atlas **level 0** must carry the *prepared* base — the same image level 1
/// was downsampled from — not the raw decoded PNG.
///
/// Vanilla's `MipmapGenerator.generateMipLevels` runs `TextureUtil.solidify`
/// on `currentMips[0]` **in place** and then sets `result[0] = currentMips[0]`,
/// and that same `NativeImage` is what `SpriteContents.uploadFirstFrame`
/// uploads at level 0. This builder used to blit the raw image at level 0 while
/// levels 1..n came from the solidified copy, so level 0 was the one level in
/// the chain that disagreed with its own successor.
///
/// The preparation never touches alpha, so no cutout decision moves. What moves
/// is the RGB a *bilinear* tap picks up beside a cutout edge — the model
/// sampler is `min_filter: Linear`/`mipmap_filter: Linear`, so at any LOD
/// between 0 and 1 a tap straddling the edge blends the transparent
/// neighbour's RGB in, and raw that is transparent **black**.
///
/// Both hypotheses are computed from outside: the raw image writes
/// `[9, 9, 9, 0]` into the hole (the value `holed` put there), the prepared one
/// writes `[10, 20, 30, 0]` (the surrounding opaque colour, alpha still `0`).
#[test]
fn atlas_level_zero_carries_the_solidified_base_the_mip_chain_was_built_from() {
    let opaque = [10u8, 20, 30, 255];
    let raw_hole = [9u8, 9, 9];

    let mut b = AtlasBuilder::new().with_mip_levels(2);
    b.add_texture(loc("minecraft:block/holed"), holed(opaque, raw_hole), None);
    let atlas = b.build().unwrap();
    let sprite = atlas.sprite(&loc("minecraft:block/holed")).unwrap();
    let hole = texel_at(&atlas.rgba, atlas.width, sprite.x + 1, sprite.y + 1);
    assert_eq!(
        hole,
        [opaque[0], opaque[1], opaque[2], 0],
        "with mips requested, level 0 must be the solidified base: the hole's RGB is its \
         nearest opaque neighbour and its alpha is still 0. The raw base would give \
         {:?}",
        [raw_hole[0], raw_hole[1], raw_hole[2], 0]
    );

    // Control: no mips requested means no chain, so nothing prepares the base
    // and level 0 stays byte-for-byte the decoded PNG. This is what keeps every
    // non-mipped atlas in the tree (GUI, items, particles) unchanged, and it is
    // what proves the assertion above is measuring the preparation rather than
    // something the builder does to every sprite regardless.
    let mut b2 = AtlasBuilder::new();
    b2.add_texture(loc("minecraft:block/holed"), holed(opaque, raw_hole), None);
    let flat = b2.build().unwrap();
    let sprite2 = flat.sprite(&loc("minecraft:block/holed")).unwrap();
    assert_eq!(
        texel_at(&flat.rgba, flat.width, sprite2.x + 1, sprite2.y + 1),
        [raw_hole[0], raw_hole[1], raw_hole[2], 0],
        "an atlas with no mip levels must still blit the raw image at level 0"
    );

    // And level 1 must be the downsample of the *same* prepared base, so level
    // 0 and level 1 now agree about what sits under the hole. Level 1's texel
    // (0, 0) averages the 2x2 quad containing the hole; with the hole
    // solidified every one of the four carries `opaque`'s RGB.
    let l1 = atlas.mip(1).unwrap();
    let t = texel_at(l1.rgba, l1.width, sprite.x >> 1, sprite.y >> 1);
    assert_eq!(
        [t[0], t[1], t[2]],
        [opaque[0], opaque[1], opaque[2]],
        "level 1 must average the solidified base, not a black hole"
    );
}
