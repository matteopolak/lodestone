//! What the terrain atlas's **uploaded** mip chain actually contains, level by
//! level, at every `mipmapLevels` setting a player can select.
//!
//! # Why this exists
//!
//! `atlas_mip_edge_bleed_gate` already asks "can one sprite bleed into its
//! neighbour", using two hand-made sprites and one edge sample. It cannot see
//! this, because it never asks the *other* question: whether the region beside
//! a sprite was written at all, at every level of the chain that reaches the
//! GPU.
//!
//! It had not been written. `atlas_mip_levels` used to fall back to
//! [`generate_isolated_mips`] over the stitched image whenever an [`Atlas`]
//! carried no pyramid of its own — which is exactly what `mipmapLevels = 0`
//! produces, a legal position on vanilla's own `IntRange(0, 4)` slider. That
//! generator allocates each level zero-filled and writes only the sprite
//! rectangles, which carry no padding, so from level 1 down every texel between
//! sprites is transparent **black**; and it asks for `mip_level_count(2048,
//! 2048)` = 11 extra levels, so past level 4 a 16x16 sprite is under one texel
//! and unrelated sprites collide in the destination. Measured beside
//! `block/stone` before the fix: `0x00000000` immediately outside the sprite at
//! levels 1-4, then `2a766f`, `9b9c62`, and a flat `f7c526` at the top.
//!
//! The block atlas's sampler is `min_filter: Linear`, so a tap at a face's own
//! edge blends that in. On an alpha-tested quad it drops the filtered alpha
//! under `model.wgsl`'s cutout threshold — a background-coloured pinprick at a
//! block edge, from a fully opaque sprite. On one that bypasses the test it
//! drags the colour toward black. Either way the contaminated share of a face
//! is half a texel out of `16 >> level`: 3% at level 0, 25% at level 3, 50% at
//! level 4, and then it stops growing because the level clamps — thin near,
//! thicker further, then constant.
//!
//! # What is asserted, and why these two things
//!
//! **The level count is vanilla's arithmetic**, not a floor or a bound:
//! `TextureAtlas.createTexture` asks for `mipLevel + 1` levels, so
//! `mipmapLevels = n` must upload exactly `n + 1`. Predicting the number (and
//! not merely "more than one") is what separates the fix from the bug — the bug
//! uploaded **11** at `n = 0`.
//!
//! **Every texel orthogonally outside a sprite's rect, at every uploaded level,
//! equals that sprite's own nearest edge texel.** That is the extrusion vanilla
//! gets from `TextureAtlas.uploadInitialContents` drawing over the *padded*
//! rect while `animate_sprite.vsh` pushes the sprite UV outward against a
//! `CLAMP_TO_EDGE` scratch texture. Equality with the edge texel is a stronger
//! claim than "opaque" or "not black": a neighbouring sprite's texels are
//! usually opaque too.
//!
//! Mismatches are collected and asserted on **as a set**, so a run reports every
//! failing (sprite, level, side) rather than aborting on the first one.
//!
//! `#[ignore]`d because it needs a real `client.jar`; it fails closed rather
//! than skipping. The level-count arm below it is hermetic and always runs.
//!
//! ```text
//! cargo test -p lodestone-render --test atlas_uploaded_chain_gutter_gate -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use lodestone_assets::{Atlas, AtlasBuilder, Image, ResourceLocation, ResourceManager, ZipSource};
use lodestone_render::{BlockAtlas, BlocksJsonRegistry, texture::atlas_mip_levels};

/// Vanilla's `IntRange(0, 4)` for `options.mipmapLevels`, which is also
/// `menu::options::INT_RANGE_SLIDERS`' row and `Options::mipmap_levels`' clamp.
/// Every one of these is a position a player can leave the slider on.
const SELECTABLE_MIP_LEVELS: [u32; 5] = [0, 1, 2, 3, 4];

/// Sprites checked by name. All are ordinary opaque building blocks present in
/// every vanilla pack, so a failure here is about the atlas rather than about
/// one texture's own alpha.
const SPRITES: [&str; 5] = [
    "minecraft:block/stone",
    "minecraft:block/andesite",
    "minecraft:block/dirt",
    "minecraft:block/oak_planks",
    "minecraft:block/cobblestone",
];

fn cache_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .join(".cache/mc/26.2")
}

/// Fails closed: an explicitly-run `#[ignore]`d gate must never pass without
/// its jar.
fn jar_atlas(mip_levels: u32) -> BlockAtlas {
    let root = cache_root();
    let jar = root.join("client.jar");
    let zip = ZipSource::open(&jar)
        .unwrap_or_else(|e| panic!("this gate needs {}: {e}", jar.display()));
    let manager = ResourceManager::new(vec![Box::new(zip)]);
    let report = root.join("generated/reports/blocks.json");
    let bytes = std::fs::read(&report).unwrap_or_else(|e| panic!("read {}: {e}", report.display()));
    let registry = BlocksJsonRegistry::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("load {}: {e}", report.display()));
    BlockAtlas::build_with_mip_levels(&manager, &registry, mip_levels)
        .unwrap_or_else(|e| panic!("build atlas at mip_levels={mip_levels}: {e}"))
}

fn texel(level: &lodestone_render::MipLevel, x: u32, y: u32) -> [u8; 4] {
    let i = ((y as usize) * (level.width as usize) + x as usize) * 4;
    [
        level.rgba[i],
        level.rgba[i + 1],
        level.rgba[i + 2],
        level.rgba[i + 3],
    ]
}

/// One disagreement between a gutter texel and the sprite edge it must
/// replicate. Carries the level's own dimensions and both texels so a failure
/// says *what* it found, not just that it found something.
#[derive(Debug)]
struct Mismatch {
    sprite: &'static str,
    requested_levels: u32,
    level: u32,
    level_size: (u32, u32),
    side: &'static str,
    got: [u8; 4],
    want: [u8; 4],
}

#[test]
#[ignore = "requires the vanilla client.jar under .cache/mc/26.2"]
fn every_uploaded_level_replicates_the_sprite_edge_into_its_gutter() {
    let mut mismatches: Vec<Mismatch> = Vec::new();
    let mut checks = 0usize;
    let mut level_counts: Vec<(u32, usize)> = Vec::new();

    for requested in SELECTABLE_MIP_LEVELS {
        let block_atlas = jar_atlas(requested);
        let atlas = block_atlas.atlas();
        let uploaded = atlas_mip_levels(atlas);
        level_counts.push((requested, uploaded.len()));

        for name in SPRITES {
            let loc = ResourceLocation::parse(name).expect("sprite name parses");
            let sprite = atlas
                .sprite(&loc)
                .unwrap_or_else(|| panic!("{name} is not in the stitched block atlas"));
            for (lvl, level) in uploaded.iter().enumerate() {
                let lvl = lvl as u32;
                let sx = sprite.x >> lvl;
                let sy = sprite.y >> lvl;
                let sw = (sprite.width >> lvl).max(1);
                let sh = (sprite.height >> lvl).max(1);
                // Mid-edge on each side, and the texel one step outside it.
                let mx = sx + sw / 2;
                let my = sy + sh / 2;
                // `checked_sub` rather than `sx - 1`: a level deep enough to put
                // a sprite at the atlas origin has no texel on that side, and a
                // probe that panicked there would abort the run before it could
                // report the levels it *had* looked at.
                let probes: [(&'static str, Option<(u32, u32)>, (u32, u32)); 4] = [
                    ("left", sx.checked_sub(1).map(|x| (x, my)), (sx, my)),
                    ("right", Some((sx + sw, my)), (sx + sw - 1, my)),
                    ("top", sy.checked_sub(1).map(|y| (mx, y)), (mx, sy)),
                    ("bottom", Some((mx, sy + sh)), (mx, sy + sh - 1)),
                ];
                for (side, outside, edge) in probes {
                    let Some(outside) = outside else { continue };
                    if outside.0 >= level.width || outside.1 >= level.height {
                        continue;
                    }
                    checks += 1;
                    let got = texel(level, outside.0, outside.1);
                    let want = texel(level, edge.0, edge.1);
                    if got != want {
                        mismatches.push(Mismatch {
                            sprite: name,
                            requested_levels: requested,
                            level: lvl,
                            level_size: (level.width, level.height),
                            side,
                            got,
                            want,
                        });
                    }
                }
            }
        }
    }

    println!("uploaded level counts (mipmapLevels, levels): {level_counts:?}");
    println!("gutter texels compared: {checks}");

    assert!(
        checks > 0,
        "no gutter texel was compared — the probe found nothing to look at, \
         which is a failure to run, not a pass"
    );
    assert!(
        mismatches.is_empty(),
        "{} of {checks} gutter texels do not replicate their sprite's own edge; \
         a Linear-minified tap at a face edge reads these:\n{:#?}",
        mismatches.len(),
        mismatches
    );

    // Vanilla's texture-atlas creation function asks for
    // `mipLevel + 1`. Predicted exactly, because the defect this gate was
    // written for uploaded 11 at `mipmapLevels = 0` — a floor of "at least one"
    // would have passed it.
    let expected: Vec<(u32, usize)> = SELECTABLE_MIP_LEVELS
        .iter()
        .map(|&n| (n, n as usize + 1))
        .collect();
    assert_eq!(
        level_counts, expected,
        "mipmapLevels = n must upload exactly n + 1 levels, as vanilla's \
         texture-atlas creation function (.., mipLevel + 1) does"
    );
}

/// The level-count rule again, hermetically: no jar, no GPU, so it runs in the
/// default suite and cannot go quiet when `.cache` is absent.
///
/// Two sprites rather than one: a single-sprite atlas would place its sprite at
/// the origin and hide any layout arithmetic.
#[test]
fn an_atlas_uploads_exactly_the_levels_it_carries_and_never_invents_more() {
    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Image {
        Image {
            width: w,
            height: h,
            rgba: rgba.iter().copied().cycle().take((w * h * 4) as usize).collect(),
        }
    }
    fn build(mip_levels: u32) -> Atlas {
        let mut builder = AtlasBuilder::new().with_width(256);
        if mip_levels > 0 {
            builder = builder
                .with_mip_levels(mip_levels)
                .with_padding(1 << mip_levels);
        }
        builder.add_texture(
            ResourceLocation::parse("test:a").expect("location"),
            solid(16, 16, [200, 30, 30, 255]),
            None,
        );
        builder.add_texture(
            ResourceLocation::parse("test:b").expect("location"),
            solid(16, 16, [30, 30, 200, 255]),
            None,
        );
        builder.build().expect("atlas builds")
    }

    for requested in SELECTABLE_MIP_LEVELS {
        let atlas = build(requested);
        assert_eq!(
            atlas_mip_levels(&atlas).len(),
            requested as usize + 1,
            "mipmapLevels = {requested} must upload exactly {} levels; a 256-wide \
             atlas would otherwise invent {} of them",
            requested + 1,
            lodestone_render::texture::mip_level_count(256, 256),
        );
    }
}
