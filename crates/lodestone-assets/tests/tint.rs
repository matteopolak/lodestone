//! Hermetic tests for biome tint resolution (colormaps, constants, seams).

use lodestone_assets::Image;
use lodestone_assets::tint::{
    BiomeTint, Colormap, GrassColorModifier, Rgb, TintKind, colors, redstone_power_color,
    vanilla_particle_tint_kind, vanilla_tint_kind,
};
use lodestone_model::{BlockPos, Identifier};
use std::collections::BTreeMap;

/// Builds a synthetic 256x256 colormap whose pixel at (x, y) encodes its own
/// coordinates as 0x00XXYY, so a sample can be checked against the exact index.
fn coord_colormap() -> Colormap {
    let mut rgba = vec![0u8; 256 * 256 * 4];
    for y in 0..256u32 {
        for x in 0..256u32 {
            let i = ((y * 256 + x) * 4) as usize;
            rgba[i] = x as u8;
            rgba[i + 1] = y as u8;
            rgba[i + 2] = 0;
            rgba[i + 3] = 255;
        }
    }
    let img = Image {
        width: 256,
        height: 256,
        rgba,
    };
    Colormap::from_image(&img, 0xABCDEF).unwrap()
}

#[test]
fn colormap_sample_matches_vanilla_index() {
    let cm = coord_colormap();
    // ColorMapColorUtil: rain *= temp; x=(1-temp)*255; y=(1-rain)*255.
    // temp=0.5, downfall=1.0 -> rain=0.5 -> x=127, y=127.
    let c = cm.sample(0.5, 1.0);
    assert_eq!((c >> 16) & 0xFF, 127, "x index");
    assert_eq!((c >> 8) & 0xFF, 127, "y index");

    // temp=1.0, downfall=1.0 -> rain=1.0 -> x=0, y=0.
    let hot_wet = cm.sample(1.0, 1.0);
    assert_eq!(hot_wet, 0x000000);

    // temp=0.0 -> x=255, rain=0 -> y=255.
    let cold_dry = cm.sample(0.0, 0.0);
    assert_eq!((cold_dry >> 16) & 0xFF, 255);
    assert_eq!((cold_dry >> 8) & 0xFF, 255);
}

#[test]
fn colormap_sample_clamps_out_of_range_inputs() {
    let cm = coord_colormap();
    // Values outside [0,1] must clamp, never index out of bounds.
    assert_eq!(cm.sample(5.0, 5.0), cm.sample(1.0, 1.0));
    assert_eq!(cm.sample(-5.0, -5.0), cm.sample(0.0, 0.0));
}

#[test]
fn colormap_default_used_when_smaller_than_index() {
    // A 1x1 colormap: any non-zero index falls back to the default colour.
    let img = Image {
        width: 1,
        height: 1,
        rgba: vec![1, 2, 3, 255],
    };
    let cm = Colormap::from_image(&img, 0xABCDEF).unwrap();
    // temp=0,downfall=0 -> index 255<<8|255, out of a 1-pixel map -> default.
    assert_eq!(cm.sample(0.0, 0.0), 0xABCDEF);
}

#[test]
fn verified_colour_constants() {
    // Verified against Mojang server source (FoliageColor / DryFoliageColor).
    assert_eq!(colors::FOLIAGE_DEFAULT, 0x48B518);
    assert_eq!(colors::FOLIAGE_EVERGREEN, 0x619961);
    assert_eq!(colors::FOLIAGE_BIRCH, 0x80A755);
    assert_eq!(colors::FOLIAGE_MANGROVE, 0x92C648);
    assert_eq!(colors::DRY_FOLIAGE_DEFAULT, 0x5C3C32);
}

#[test]
fn dark_forest_modifier_matches_source() {
    // ARGB.opaque((base & 0xFEFEFE) + 0x28340A >> 1).
    let base: Rgb = 0x88AA44;
    let out = GrassColorModifier::DarkForest.modify(base, 0.0);
    let expected = (((base & 0xFEFEFE) + 0x28340A) >> 1) & 0xFFFFFF;
    assert_eq!(out, expected);
}

#[test]
fn swamp_modifier_two_tone_by_noise() {
    // groundValue < -0.1 -> 0x4C763C else 0x6A7039.
    assert_eq!(GrassColorModifier::Swamp.modify(0x112233, -0.5), 0x4C763C);
    assert_eq!(GrassColorModifier::Swamp.modify(0x112233, 0.0), 0x6A7039);
}

#[test]
fn none_modifier_is_identity() {
    assert_eq!(GrassColorModifier::None.modify(0x123456, 0.0), 0x123456);
}

#[test]
fn redstone_power_ramp_endpoints() {
    // power 0 and 15, from the verified RedStoneWireBlock ramp.
    let off = redstone_power_color(0);
    let full = redstone_power_color(15);
    // power 0: red = 0*0.6 + 0.3 = 0.3 -> 77; green=blue=0.
    assert_eq!((off >> 16) & 0xFF, 77);
    assert_eq!(off & 0xFFFF, 0);
    // power 15: red = 0.6 + 0.4 = 1.0 -> 255.
    assert_eq!((full >> 16) & 0xFF, 255);
    // saturating power ramps green up at max: green = clamp(1*0.7-0.5,0,1)=0.2 -> 51.
    assert_eq!((full >> 8) & 0xFF, 51);
}

/// A fake biome that returns fixed climate and overrides for testing resolve.
#[derive(Debug)]
struct FakeBiome {
    temp: f32,
    downfall: f32,
    water: Rgb,
    grass_override: Option<Rgb>,
    modifier: GrassColorModifier,
}

impl BiomeTint for FakeBiome {
    fn temperature(&self, _pos: BlockPos) -> f32 {
        self.temp
    }
    fn downfall(&self, _pos: BlockPos) -> f32 {
        self.downfall
    }
    fn water_color(&self, _pos: BlockPos) -> Rgb {
        self.water
    }
    fn grass_override(&self, _pos: BlockPos) -> Option<Rgb> {
        self.grass_override
    }
    fn grass_modifier(&self, _pos: BlockPos) -> GrassColorModifier {
        self.modifier
    }
}

fn colormaps_for_test() -> lodestone_assets::tint::Colormaps {
    lodestone_assets::tint::Colormaps {
        grass: coord_colormap(),
        foliage: coord_colormap(),
        dry_foliage: coord_colormap(),
    }
}

#[test]
fn resolve_grass_uses_override_and_modifier() {
    let maps = colormaps_for_test();
    let pos = BlockPos::new(0, 0, 0);
    let biome = FakeBiome {
        temp: 0.5,
        downfall: 1.0,
        water: 0x3F76E4,
        grass_override: Some(0x88AA44),
        modifier: GrassColorModifier::DarkForest,
    };
    let got = maps.resolve(TintKind::Grass, &biome, pos).unwrap();
    let expected = (((0x88AA44u32 & 0xFEFEFE) + 0x28340A) >> 1) & 0xFFFFFF;
    assert_eq!(got, expected);
}

#[test]
fn resolve_grass_from_colormap_when_no_override() {
    let maps = colormaps_for_test();
    let pos = BlockPos::new(0, 0, 0);
    let biome = FakeBiome {
        temp: 0.5,
        downfall: 1.0,
        water: 0,
        grass_override: None,
        modifier: GrassColorModifier::None,
    };
    let got = maps.resolve(TintKind::Grass, &biome, pos).unwrap();
    // colormap coord (127,127) -> 0x00_7F_7F_00.
    assert_eq!((got >> 16) & 0xFF, 127);
}

#[test]
fn resolve_water_none_and_constant_and_redstone() {
    let maps = colormaps_for_test();
    let pos = BlockPos::new(0, 0, 0);
    let biome = FakeBiome {
        temp: 0.5,
        downfall: 0.5,
        water: 0x3F76E4,
        grass_override: None,
        modifier: GrassColorModifier::None,
    };
    assert_eq!(maps.resolve(TintKind::Water, &biome, pos), Some(0x3F76E4));
    assert_eq!(maps.resolve(TintKind::None, &biome, pos), None);
    assert_eq!(
        maps.resolve(TintKind::Constant(0x123456), &biome, pos),
        Some(0x123456)
    );
    assert_eq!(
        maps.resolve(TintKind::RedstonePower(15), &biome, pos),
        Some(redstone_power_color(15))
    );
}

#[test]
fn vanilla_classification_known_blocks() {
    let props = BTreeMap::new();
    let id = |s: &str| s.parse::<Identifier>().unwrap();

    assert_eq!(
        vanilla_tint_kind(&id("minecraft:oak_leaves"), 0, &props),
        TintKind::Foliage
    );
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:spruce_leaves"), 0, &props),
        TintKind::Constant(colors::FOLIAGE_EVERGREEN)
    );
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:birch_leaves"), 0, &props),
        TintKind::Constant(colors::FOLIAGE_BIRCH)
    );
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:grass_block"), 0, &props),
        TintKind::Grass
    );
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:short_grass"), 0, &props),
        TintKind::Grass
    );
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:water_cauldron"), 0, &props),
        TintKind::Water
    );

    let mut rprops = BTreeMap::new();
    rprops.insert("power".to_string(), "9".to_string());
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:redstone_wire"), 0, &rprops),
        TintKind::RedstonePower(9)
    );

    // Unknown block: no tint rather than a guess.
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:stone"), 0, &props),
        TintKind::None
    );

    // Verified corrections from client BlockColors.createDefault():
    // mangrove leaves use the foliage colormap, not a constant.
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:mangrove_leaves"), 0, &props),
        TintKind::Foliage
    );
    // leaf_litter uses the dry-foliage colormap.
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:leaf_litter"), 0, &props),
        TintKind::DryFoliage
    );
    // pink_petals/wildflowers: index 0 blank, index 1 grass.
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:pink_petals"), 0, &props),
        TintKind::None
    );
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:pink_petals"), 1, &props),
        TintKind::Grass
    );
    // lily pad: constant in-world colour.
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:lily_pad"), 0, &props),
        TintKind::Constant(colors::LILY_PAD_IN_WORLD)
    );
    // Carries tintindex 0 in its model but is NOT registered → untinted.
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:cherry_leaves"), 0, &props),
        TintKind::None
    );
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:pale_oak_leaves"), 0, &props),
        TintKind::None
    );
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:bamboo"), 0, &props),
        TintKind::None
    );

    // Growing stems fade with age: ARGB.color(age*32, 255-age*8, age*4).
    let mut sprops = BTreeMap::new();
    sprops.insert("age".to_string(), "7".to_string());
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:melon_stem"), 0, &sprops),
        TintKind::Constant(0xE0C71C)
    );
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:attached_pumpkin_stem"), 0, &props),
        TintKind::Constant(0xE0C71C)
    );
    assert_eq!(lodestone_assets::tint::stem_color(0), 0x00FF00);
}

/// `colorAsTerrainParticle` is a *different* virtual method from the in-world
/// face tint, and vanilla's `BlockTintSources` (26.2) deliberately makes the two
/// disagree for exactly two registrations. Both divergences are load-bearing on
/// screen, and both are the kind a "reuse the face tint" implementation gets
/// silently wrong:
///
/// * `grass_block` — `grassBlock()` overrides `colorAsTerrainParticle` to `-1`,
///   because `grass_block`'s `#particle` variable is `block/dirt`. Tinting it
///   throws **green dirt**.
/// * `water` / `bubble_column` — `waterParticles()` is the mirror image: `color`
///   and `colorInWorld` are `-1` (the surface is tinted by the fluid model) while
///   `colorAsTerrainParticle` returns the biome water colour.
#[test]
fn particle_tint_diverges_from_the_face_tint_exactly_where_vanilla_does() {
    let props = BTreeMap::new();
    let id = |s: &str| s.parse::<Identifier>().unwrap();

    // grass_block: grass-tinted faces, untinted particles.
    assert_eq!(
        vanilla_tint_kind(&id("minecraft:grass_block"), 0, &props),
        TintKind::Grass,
        "the face tint is unchanged by this"
    );
    assert_eq!(
        vanilla_particle_tint_kind(&id("minecraft:grass_block"), &props),
        TintKind::None,
        "grass_block's particle sprite is block/dirt; tinting it would throw green dirt"
    );

    // water / bubble_column: untinted faces, water-tinted particles.
    for block in ["minecraft:water", "minecraft:bubble_column"] {
        assert_eq!(
            vanilla_tint_kind(&id(block), 0, &props),
            TintKind::None,
            "{block}'s surface is tinted by the fluid model, not by a face tint index"
        );
        assert_eq!(
            vanilla_particle_tint_kind(&id(block), &props),
            TintKind::Water,
            "{block} registers waterParticles(), which tints particles only"
        );
    }

    // Everything else inherits colorAsTerrainParticle from colorInWorld, so the
    // two lookups must agree at layer 0. `short_grass` and `redstone_wire` are
    // the greyscale-sprite blocks the white-debris bug was reported against.
    let mut rprops = BTreeMap::new();
    rprops.insert("power".to_string(), "9".to_string());
    for (block, props) in [
        ("minecraft:short_grass", &props),
        ("minecraft:fern", &props),
        ("minecraft:tall_grass", &props),
        ("minecraft:oak_leaves", &props),
        ("minecraft:vine", &props),
        ("minecraft:sugar_cane", &props),
        ("minecraft:lily_pad", &props),
        ("minecraft:spruce_leaves", &props),
        ("minecraft:leaf_litter", &props),
        ("minecraft:redstone_wire", &rprops),
        // Untinted, and must stay so: carrying a tintindex is not being tinted.
        ("minecraft:stone", &props),
        ("minecraft:cherry_leaves", &props),
        ("minecraft:bamboo", &props),
    ] {
        assert_eq!(
            vanilla_particle_tint_kind(&id(block), props),
            vanilla_tint_kind(&id(block), 0, props),
            "{block} has no colorAsTerrainParticle override, so the two lookups must agree"
        );
    }

    // Non-vacuity: the agreeing set must actually contain tinted blocks, or the
    // loop above is satisfied by `None == None` throughout.
    assert_eq!(
        vanilla_particle_tint_kind(&id("minecraft:short_grass"), &props),
        TintKind::Grass
    );
    assert_eq!(
        vanilla_particle_tint_kind(&id("minecraft:redstone_wire"), &rprops),
        TintKind::RedstonePower(9)
    );
}
