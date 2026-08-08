//! Hermetic tests for biome tint resolution (colormaps, constants, seams).

use lodestone_assets::Image;
use lodestone_assets::tint::{
    BiomeTint, BlendRowCursor, Colormap, DEFAULT_BLEND_RADIUS, GrassColorModifier,
    MAX_BLEND_RADIUS, Rgb, TintKind, biome_effects, blend_box, colors, redstone_power_color,
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

// --- BiomeEffects table -----------------------------------------------

#[test]
fn biome_effects_accepts_namespaced_and_bare_ids() {
    let bare = biome_effects("swamp").expect("swamp is one of the 66");
    let namespaced = biome_effects("minecraft:swamp").expect("swamp is one of the 66");
    assert_eq!(bare, namespaced);
}

#[test]
fn biome_effects_unknown_id_is_none() {
    assert!(biome_effects("minecraft:not_a_real_biome").is_none());
}

#[test]
fn biome_effects_table_has_all_66_vanilla_biomes() {
    // The same 66-biome set `docs/worldgen-biomes.md`'s "66/66" gate checks,
    // read directly off the jar's `worldgen/biome/*.json` filenames.
    const NAMES: &[&str] = &[
        "badlands", "bamboo_jungle", "basalt_deltas", "beach", "birch_forest", "cherry_grove",
        "cold_ocean", "crimson_forest", "dark_forest", "deep_cold_ocean", "deep_dark",
        "deep_frozen_ocean", "deep_lukewarm_ocean", "deep_ocean", "desert", "dripstone_caves",
        "end_barrens", "end_highlands", "end_midlands", "eroded_badlands", "flower_forest",
        "forest", "frozen_ocean", "frozen_peaks", "frozen_river", "grove", "ice_spikes",
        "jagged_peaks", "jungle", "lukewarm_ocean", "lush_caves", "mangrove_swamp", "meadow",
        "mushroom_fields", "nether_wastes", "ocean", "old_growth_birch_forest",
        "old_growth_pine_taiga", "old_growth_spruce_taiga", "pale_garden", "plains", "river",
        "savanna", "savanna_plateau", "small_end_islands", "snowy_beach", "snowy_plains",
        "snowy_slopes", "snowy_taiga", "soul_sand_valley", "sparse_jungle", "stony_peaks",
        "stony_shore", "sulfur_caves", "sunflower_plains", "swamp", "taiga", "the_end",
        "the_void", "warm_ocean", "warped_forest", "windswept_forest",
        "windswept_gravelly_hills", "windswept_hills", "windswept_savanna", "wooded_badlands",
    ];
    assert_eq!(NAMES.len(), 66, "test's own list should be 66 too");
    for name in NAMES {
        assert!(biome_effects(name).is_some(), "missing biome: {name}");
    }
}

/// Every value here transcribed directly from
/// `.cache/mc/26.2/src/data/minecraft/worldgen/biome/{swamp,plains,mangrove_swamp,
/// desert}.json` — see `crates/lodestone-assets/src/tint.rs`'s `BIOME_EFFECTS`
/// doc for the full derivation.
#[test]
fn biome_effects_matches_jar_values() {
    let swamp = biome_effects("swamp").unwrap();
    assert_eq!(swamp.temperature, 0.8);
    assert_eq!(swamp.downfall, 0.9);
    assert_eq!(swamp.water_color, 0x617B64);
    assert_eq!(swamp.grass_color, None);
    assert_eq!(swamp.foliage_color, Some(0x6A7039));
    assert_eq!(swamp.dry_foliage_color, Some(0x7B5334));
    assert_eq!(swamp.grass_modifier, GrassColorModifier::Swamp);

    let plains = biome_effects("plains").unwrap();
    assert_eq!(plains.temperature, 0.8);
    assert_eq!(plains.downfall, 0.4);
    assert_eq!(plains.water_color, 0x3F76E4);
    assert_eq!(plains.grass_color, None);
    assert_eq!(plains.grass_modifier, GrassColorModifier::None);

    let mangrove = biome_effects("mangrove_swamp").unwrap();
    assert_eq!(mangrove.water_color, 0x3A7A6A);
    assert_eq!(mangrove.foliage_color, Some(0x8DB127));
    assert_eq!(mangrove.grass_modifier, GrassColorModifier::Swamp);

    let desert = biome_effects("desert").unwrap();
    assert_eq!(desert.temperature, 2.0);
    assert_eq!(desert.downfall, 0.0);
    assert_eq!(desert.water_color, 0x3F76E4);
}

// --- blend_box -----------------------------------------------------------

#[test]
fn blend_box_radius_zero_is_the_single_sample() {
    // Vanilla's `dist == 0` fast path: no averaging at all.
    let c = blend_box(5, 9, 0, |x, z| {
        assert_eq!((x, z), (5, 9), "radius 0 must sample exactly (x, z), nothing else");
        0x102030
    });
    assert_eq!(c, 0x102030);
}

#[test]
fn blend_box_uniform_field_is_the_identity() {
    // A field that returns the same colour everywhere must blend to that
    // colour exactly, at the default radius — the control every biome-tint
    // gate should reduce to away from a boundary.
    let c = blend_box(0, 0, DEFAULT_BLEND_RADIUS, |_, _| 0x40_80_C0);
    assert_eq!(c, 0x40_80_C0);
}

#[test]
fn blend_box_averages_a_hard_boundary_and_counts_every_sample() {
    // A hard boundary at x >= 0 (red) vs x < 0 (blue), sampled at radius 2
    // centred on the boundary itself: 25 samples, of which 15 have x >= 0
    // (columns x = 0, 1, 2) and 10 have x < 0 (columns x = -2, -1). Verifies
    // both the exact vanilla kernel shape (5x5 around the centre, not
    // (2r)x(2r) or clipped) and the integer (floor) division.
    let mut calls = 0u32;
    let c = blend_box(0, 0, 2, |x, _z| {
        calls += 1;
        if x >= 0 { 0xFF0000 } else { 0x0000FF }
    });
    assert_eq!(calls, 25, "5x5 = 25 samples at radius 2");
    let red_count = 15u32; // x in {0,1,2} * 5 rows
    let blue_count = 10u32; // x in {-2,-1} * 5 rows
    let expected_r = (0xFFu32 * red_count) / 25;
    let expected_b = (0xFFu32 * blue_count) / 25;
    assert_eq!((c >> 16) & 0xFF, expected_r);
    assert_eq!(c & 0xFF, expected_b);
    assert_eq!((c >> 8) & 0xFF, 0, "no green in either source colour");
}

#[test]
fn blend_box_samples_every_cell_of_the_kernel_exactly_once() {
    // A hash-based per-cell colour: if any cell were sampled twice or skipped,
    // the arithmetic-mean channel wouldn't match a hand-summed total computed
    // independently of `blend_box`'s own loop.
    let mut total = [0u32; 3];
    let mut count = 0u32;
    let c = blend_box(100, -50, DEFAULT_BLEND_RADIUS, |x, z| {
        count += 1;
        let r = (x.rem_euclid(256)) as u32;
        let g = (z.rem_euclid(256)) as u32;
        let b = ((x + z).rem_euclid(256)) as u32;
        total[0] += r;
        total[1] += g;
        total[2] += b;
        (r << 16) | (g << 8) | b
    });
    assert_eq!(count, 25);
    assert_eq!((c >> 16) & 0xFF, total[0] / 25);
    assert_eq!((c >> 8) & 0xFF, total[1] / 25);
    assert_eq!(c & 0xFF, total[2] / 25);
}

// --- BlendRowCursor ------------------------------------------------------
//
// `blend_box` above is the reference arm for everything here: the cursor exists
// to replace it, so the correct expected output *is* its output, and it stays
// alive in the crate for exactly that reason. `DESIGN.md` §12.128.

/// A deterministic colour field with **two** structures a blend can see: a hard
/// vertical boundary at `x = 0` in the red channel (the biome-boundary shape the
/// real caller meets) and per-cell hash noise in green/blue (so two adjacent
/// blends differ, which is what makes the identity assertions non-trivial).
fn hashed_field(x: i32, z: i32) -> Rgb {
    let h = (x as u32)
        .wrapping_mul(2_654_435_761)
        .rotate_left(13)
        ^ (z as u32).wrapping_mul(2_246_822_519);
    let r = if x < 0 { 0x20 } else { 0xC0 };
    let g = (h >> 7) & 0xFF;
    let b = (h >> 19) & 0xFF;
    (r << 16) | (g << 8) | b
}

/// The load-bearing property: **bit-identical**, not close. Vanilla is not
/// colour-managed and both tint and shade multiply in gamma space, so a blend
/// that reassociates its sum or divides early shifts colours by a byte or two —
/// invisible in a screenshot and wrong.
///
/// Every radius the vanilla option exposes, walked forward, backward, revisited
/// in place, and jumped by exactly `width - 1`, `width` and `width + 1` (the
/// three cases either side of the slide/rebuild decision) plus a jump far past
/// the window. Each visited `(x, z)` is compared against `blend_box` at the same
/// radius.
#[test]
fn blend_row_cursor_is_bit_identical_to_blend_box() {
    // Cumulative x deltas, chosen to cross every branch of the slide decision.
    // `0` revisits the same centre; the signed values slide both ways.
    const WALK: [i32; 18] = [0, 1, 1, 1, 1, 0, -1, -1, 2, 3, 4, 5, -5, 6, 7, 15, -40, 1];
    let mut checked = 0usize;
    for radius in 0..=MAX_BLEND_RADIUS {
        let width = 2 * radius + 1;
        // The interesting jumps are relative to the window width, so add them
        // per radius rather than hoping the fixed list happens to hit them.
        let walk: Vec<i32> = WALK
            .iter()
            .copied()
            .chain([width - 1, width, width + 1, -width, 0])
            .collect();
        for z in [-9, 0, 41] {
            let mut cursor = BlendRowCursor::new(radius);
            assert_eq!(cursor.radius(), radius, "radius must survive construction");
            let mut x = -3;
            for step in &walk {
                x += step;
                let want = blend_box(x, z, radius, hashed_field);
                let got = cursor.blend(x, z, hashed_field);
                assert_eq!(
                    got, want,
                    "radius {radius}, z {z}, x {x} (step {step}): the cursor blended \
                     {got:#08X} where blend_box gives {want:#08X}. These must agree to the \
                     bit — tint multiplies in gamma space, so a byte of drift is a real \
                     colour error that no screenshot shows"
                );
                checked += 1;
            }
            // A `z` change must invalidate rather than reuse the row.
            let want = blend_box(x, z + 1, radius, hashed_field);
            assert_eq!(cursor.blend(x, z + 1, hashed_field), want, "z must key the window");
            // ...and so must an explicit invalidate, which cannot change the value.
            cursor.invalidate();
            assert_eq!(cursor.blend(x, z + 1, hashed_field), want, "invalidate must be inert");
            checked += 2;
        }
    }
    assert!(
        checked > 400,
        "only {checked} positions compared; the walk did not run"
    );
}

/// Control for the test above: `blend_box` and the cursor agreeing is only
/// evidence if the field *can* disagree. A uniform field would make every one of
/// those assertions pass under any window arithmetic at all — the `world` species,
/// and the one that cannot be found by reading the assertion.
///
/// So: compare the cursor at `x` against `blend_box` at `x + 1` over the same
/// walk, and require that nearly every position separates. Predicted, not merely
/// asserted non-zero: with hash noise in two channels every adjacent pair should
/// differ, and the only way one can collide is a floor-division tie, so the floor
/// is set just below the total rather than at 1.
#[test]
fn the_blend_box_reference_arm_is_sensitive_to_a_one_column_shift() {
    let radius = DEFAULT_BLEND_RADIUS;
    let mut cursor = BlendRowCursor::new(radius);
    let (mut same, mut differ) = (0usize, 0usize);
    for x in -20..20 {
        let got = cursor.blend(x, 7, hashed_field);
        if got == blend_box(x + 1, 7, radius, hashed_field) {
            same += 1;
        } else {
            differ += 1;
        }
    }
    assert_eq!(same + differ, 40, "the loop must visit 40 positions");
    assert!(
        differ >= 38,
        "only {differ} of 40 positions distinguish a one-column shift ({same} collided). The \
         hashed field is too flat to prove anything, so \
         `blend_row_cursor_is_bit_identical_to_blend_box` would pass under wrong window \
         arithmetic too"
    );
    // And the *uniform* case, which must collide everywhere — the negative half of
    // the same control, proving the comparison is not simply always unequal.
    let mut flat = BlendRowCursor::new(radius);
    for x in -5..5 {
        assert_eq!(
            flat.blend(x, 0, |_, _| 0x40_80_C0),
            blend_box(x + 1, 0, radius, |_, _| 0x40_80_C0),
            "a uniform field must blend identically everywhere, shift or not"
        );
    }
}

/// The reason the cursor exists, as an exact predicted count rather than "fewer".
///
/// At radius 2 the window is 5 columns of 5 samples. Entering a row costs the full
/// 25; each single-step move retires one column and admits one, so it costs 5;
/// revisiting the same centre costs 0; and a jump of `width` or more rebuilds, so
/// it costs 25 again — never *more* than `blend_box`, which is the property that
/// stops this being a regression on a hostile access pattern.
#[test]
fn blend_row_cursor_samples_five_per_step_and_never_more_than_blend_box() {
    let radius = DEFAULT_BLEND_RADIUS; // 2
    let width = (2 * radius + 1) as usize; // 5
    let full = width * width; // 25
    // `Cell`, not `&mut`: the counter has to be readable *while* the closure that
    // increments it is still alive, and `blend` holds it for the call.
    let calls = std::cell::Cell::new(0usize);
    let counting = |x: i32, z: i32| {
        calls.set(calls.get() + 1);
        hashed_field(x, z)
    };
    let mut cursor = BlendRowCursor::new(radius);

    cursor.blend(0, 0, counting);
    assert_eq!(calls.get(), full, "entering a row is the whole box");

    calls.set(0);
    cursor.blend(0, 0, counting);
    assert_eq!(calls.get(), 0, "revisiting the same centre must sample nothing");

    calls.set(0);
    for x in 1..16 {
        cursor.blend(x, 0, counting);
    }
    assert_eq!(
        calls.get(),
        15 * width,
        "15 single steps must cost 15 columns of {width} = {}, not 15 boxes of {full}",
        15 * width
    );

    // The two cases either side of the slide/rebuild decision, tracked from an
    // explicit centre rather than from arithmetic on the loop bound above — the
    // first draft of this test wrote `15 - width + 1` meaning "width - 1 away
    // from 15" and measured a one-step slide, which is what predicting the exact
    // count is for.
    let mut centre = 15i32;
    calls.set(0);
    centre -= width as i32;
    cursor.blend(centre, 0, counting);
    assert_eq!(calls.get(), full, "a jump of exactly the window width rebuilds");

    calls.set(0);
    centre += width as i32 - 1;
    cursor.blend(centre, 0, counting);
    assert_eq!(
        calls.get(),
        (width - 1) * width,
        "a jump of width-1 must slide {} columns, not rebuild {full}",
        width - 1
    );

    // The whole row, priced against the function it replaces. 16 cells:
    // 25 + 15*5 = 100 samples against 16*25 = 400.
    let row_calls = std::cell::Cell::new(0usize);
    let mut row = BlendRowCursor::new(radius);
    for x in 0..16 {
        row.blend(x, 3, |sx, sz| {
            row_calls.set(row_calls.get() + 1);
            hashed_field(sx, sz)
        });
    }
    assert_eq!(row_calls.get(), 100, "a 16-cell row must cost 100 samples");
    assert_eq!(16 * full, 400, "blend_box would cost 400 for the same row");
}

/// Radius 0 is vanilla's `dist == 0` fast path in both implementations, and the
/// cursor must not invent a window for it.
#[test]
fn blend_row_cursor_radius_zero_is_the_single_sample() {
    let mut cursor = BlendRowCursor::new(0);
    let calls = std::cell::Cell::new(0usize);
    let c = cursor.blend(5, 9, |x, z| {
        calls.set(calls.get() + 1);
        assert_eq!((x, z), (5, 9), "radius 0 must sample exactly (x, z)");
        0x102030
    });
    assert_eq!(c, 0x102030);
    assert_eq!(calls.get(), 1);
    // A negative radius clamps to 0 rather than panicking on the ring width.
    assert_eq!(BlendRowCursor::new(-3).radius(), 0);
    assert_eq!(BlendRowCursor::new(99).radius(), MAX_BLEND_RADIUS);
}
