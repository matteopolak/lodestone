//! The shell's **demo block palette**, its [`BlockClassifier`], and a procedural
//! texture atlas.
//!
//! This is deliberately version-free and self-contained: the shell must run with
//! no downloaded assets and must name no protocol version, so instead of the
//! real 26.2 block registry + `lodestone-assets` atlas it uses a tiny hand-built
//! palette. The *shape* of the data is exactly what the real pipeline needs — a
//! `state_id → Cell` classifier ([`lodestone_render::BlockClassifier`]) and a
//! GPU atlas + sprite-UV table — so swapping in the real registry later is a
//! drop-in, not a redesign. That missing bridge (real state id → baked model →
//! atlas sprite) is called out in the report as a seam the library still owes.

use lodestone_render::{BlockClassifier, Cell, SpriteId, Surface};

/// Block-state ids used by [`crate::worldgen`]. These are the shell's own tiny
/// namespace, unrelated to any real protocol's ids.
pub mod id {
    /// Empty / non-rendered.
    pub const AIR: u32 = 0;
    /// Stone.
    pub const STONE: u32 = 1;
    /// Dirt.
    pub const DIRT: u32 = 2;
    /// Grass block (grassy top, dirt bottom, grassy sides).
    pub const GRASS: u32 = 3;
    /// Sand.
    pub const SAND: u32 = 4;
    /// Water (rendered opaque in this demo).
    pub const WATER: u32 = 5;
    /// Log (bark sides, ringed top/bottom).
    pub const LOG: u32 = 6;
    /// Leaves.
    pub const LEAVES: u32 = 7;
    /// Bedrock.
    pub const BEDROCK: u32 = 8;
    /// Gravel (ocean floor / surface-rule result).
    pub const GRAVEL: u32 = 9;
}

/// Sprite (atlas tile) indices. One per distinct texture.
mod sprite {
    pub const STONE: u16 = 0;
    pub const DIRT: u16 = 1;
    pub const GRASS_TOP: u16 = 2;
    pub const GRASS_SIDE: u16 = 3;
    pub const SAND: u16 = 4;
    pub const WATER: u16 = 5;
    pub const LOG_SIDE: u16 = 6;
    pub const LOG_TOP: u16 = 7;
    pub const LEAVES: u16 = 8;
    pub const BEDROCK: u16 = 9;
    pub const GRAVEL: u16 = 10;
    pub const COUNT: u16 = 11;
}

/// Face order matches [`lodestone_render::Face`]:
/// `[NegX, PosX, NegY, PosY, NegZ, PosZ]` (index 2 = bottom, 3 = top).
#[derive(Debug, Clone, Copy)]
pub struct Block {
    /// Block-state id.
    pub id: u32,
    /// Human name, used by the debug overlay / logging.
    pub name: &'static str,
    /// Per-face sprite indices.
    pub sprites: [u16; 6],
}

const fn uniform(name: &'static str, id: u32, s: u16) -> Block {
    Block {
        id,
        name,
        sprites: [s, s, s, s, s, s],
    }
}

/// The demo palette (excludes air).
const PALETTE: &[Block] = &[
    uniform("stone", id::STONE, sprite::STONE),
    uniform("dirt", id::DIRT, sprite::DIRT),
    Block {
        id: id::GRASS,
        name: "grass_block",
        // NegX,PosX,NegY(bottom),PosY(top),NegZ,PosZ
        sprites: [
            sprite::GRASS_SIDE,
            sprite::GRASS_SIDE,
            sprite::DIRT,
            sprite::GRASS_TOP,
            sprite::GRASS_SIDE,
            sprite::GRASS_SIDE,
        ],
    },
    uniform("sand", id::SAND, sprite::SAND),
    uniform("water", id::WATER, sprite::WATER),
    Block {
        id: id::LOG,
        name: "log",
        sprites: [
            sprite::LOG_SIDE,
            sprite::LOG_SIDE,
            sprite::LOG_TOP,
            sprite::LOG_TOP,
            sprite::LOG_SIDE,
            sprite::LOG_SIDE,
        ],
    },
    uniform("leaves", id::LEAVES, sprite::LEAVES),
    uniform("bedrock", id::BEDROCK, sprite::BEDROCK),
    uniform("gravel", id::GRAVEL, sprite::GRAVEL),
];

/// The demo palette (all non-air blocks).
#[must_use]
pub fn palette() -> &'static [Block] {
    PALETTE
}

/// Look up a block by id.
#[must_use]
pub fn block(id: u32) -> Option<&'static Block> {
    PALETTE.iter().find(|b| b.id == id)
}

/// The shell's classifier: resolves a demo block-state id into a render [`Cell`].
///
/// Crucially, air is a **lit but empty** cell (not [`Cell::EMPTY`]) so that the
/// faces of neighbouring blocks sample real light and don't render black — the
/// "air must carry light" hazard the render crate documents.
#[derive(Debug, Default)]
pub struct DemoClassifier;

impl BlockClassifier for DemoClassifier {
    fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell {
        match block(state_id) {
            None => Cell {
                occludes: false,
                surface: None,
                block_light,
                sky_light,
            },
            Some(b) => Cell {
                occludes: true,
                surface: Some(Surface {
                    sprites: b.sprites.map(SpriteId),
                }),
                block_light,
                sky_light,
            },
        }
    }
}

/// One 16×16 atlas tile.
const TILE: u32 = 16;

/// Base RGB colour per sprite index.
fn base_color(s: u16) -> [u8; 3] {
    match s {
        sprite::STONE => [124, 124, 124],
        sprite::DIRT => [134, 96, 67],
        sprite::GRASS_TOP => [91, 153, 74],
        sprite::GRASS_SIDE => [120, 130, 74],
        sprite::SAND => [214, 203, 152],
        sprite::WATER => [54, 96, 196],
        sprite::LOG_SIDE => [102, 81, 51],
        sprite::LOG_TOP => [160, 130, 86],
        sprite::LEAVES => [56, 110, 46],
        sprite::BEDROCK => [64, 64, 68],
        sprite::GRAVEL => [126, 120, 116],
        _ => [255, 0, 255],
    }
}

/// Build the procedural atlas: `COUNT` 16×16 tiles laid left-to-right in a
/// single row whose **width is padded up to a power of two**.
///
/// The padding is not cosmetic. `lodestone_render`'s isolated-mip generator
/// floors each sprite's origin and the atlas width independently at every mip
/// level (`sprite.x >> level` vs `width >> level`); for a tightly-packed
/// non-power-of-two width the last sprite's floored origin can land exactly on
/// the floored row width and index one texel past the destination buffer. A
/// power-of-two width keeps `sprite.x >> level < width >> level` at every level,
/// so no sprite ever writes out of bounds. (Reported as a robustness gap in the
/// render crate — its own tests only exercise a 16×16 single-sprite atlas.)
///
/// Returns [`AtlasData`] where `uv_table[i]` is
/// `[uv_min.x, uv_min.y, uv_size.x, uv_size.y]` for sprite `i` — the exact
/// layout [`lodestone_render::block::sprite_uv_buffer`] expects.
#[must_use]
pub fn build_atlas() -> AtlasData {
    let count = u32::from(sprite::COUNT);
    let width = (count * TILE).next_power_of_two();
    let height = TILE;
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    for s in 0..sprite::COUNT {
        let base = base_color(s);
        let ox = u32::from(s) * TILE;
        for ty in 0..TILE {
            for tx in 0..TILE {
                // A subtle deterministic per-texel dither so surfaces read as
                // textured, not flat — also makes readback variation visible.
                let n = ((tx.wrapping_mul(7) ^ ty.wrapping_mul(13)).wrapping_add(u32::from(s) * 5)
                    % 24) as i32
                    - 12;
                let px = ((ty * width) + ox + tx) as usize * 4;
                for c in 0..3 {
                    rgba[px + c] = (i32::from(base[c]) + n).clamp(0, 255) as u8;
                }
                rgba[px + 3] = 255;
            }
        }
    }

    let mut sprite_rects = Vec::with_capacity(sprite::COUNT as usize);
    let mut uv_table = Vec::with_capacity(sprite::COUNT as usize);
    let w = width as f32;
    for s in 0..sprite::COUNT {
        let ox = u32::from(s) * TILE;
        sprite_rects.push(lodestone_render::SpriteRect {
            x: ox,
            y: 0,
            w: TILE,
            h: TILE,
        });
        uv_table.push([ox as f32 / w, 0.0, TILE as f32 / w, 1.0]);
    }

    AtlasData {
        width,
        height,
        rgba,
        sprite_rects,
        uv_table,
    }
}

/// CPU-side atlas payload, ready to upload with
/// [`lodestone_render::GpuAtlas::from_rgba`].
#[derive(Debug, Clone)]
pub struct AtlasData {
    /// Atlas width in pixels.
    pub width: u32,
    /// Atlas height in pixels.
    pub height: u32,
    /// Tightly packed RGBA8 pixels.
    pub rgba: Vec<u8>,
    /// Per-sprite rectangles (for isolated mip generation).
    pub sprite_rects: Vec<lodestone_render::SpriteRect>,
    /// Per-sprite UV rectangles for the shader's sprite-UV storage buffer.
    pub uv_table: Vec<[f32; 4]>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_is_lit_but_empty() {
        let c = DemoClassifier.classify(id::AIR, 3, 12);
        assert!(!c.occludes);
        assert!(c.surface.is_none());
        assert_eq!(c.sky_light, 12, "air must carry light");
    }

    #[test]
    fn grass_has_distinct_top_and_bottom() {
        let c = DemoClassifier.classify(id::GRASS, 0, 15);
        let s = c.surface.expect("grass has a surface");
        // index 3 = PosY (top) is grass_top; index 2 = NegY (bottom) is dirt.
        assert_eq!(s.sprites[3], SpriteId(sprite::GRASS_TOP));
        assert_eq!(s.sprites[2], SpriteId(sprite::DIRT));
        assert_ne!(s.sprites[3], s.sprites[2]);
    }

    #[test]
    fn atlas_layout_is_consistent() {
        let a = build_atlas();
        assert_eq!(
            a.width,
            (u32::from(sprite::COUNT) * TILE).next_power_of_two()
        );
        assert!(
            a.width.is_power_of_two(),
            "width must be pow2 for safe mips"
        );
        assert_eq!(a.rgba.len(), (a.width * a.height * 4) as usize);
        assert_eq!(a.uv_table.len(), sprite::COUNT as usize);
        // First sprite starts at u=0, each spans exactly one tile of the width.
        assert!((a.uv_table[0][0] - 0.0).abs() < 1e-6);
        assert!((a.uv_table[0][2] - TILE as f32 / a.width as f32).abs() < 1e-6);
        // No sprite's floored origin can reach the floored row width at any mip
        // level — the invariant that keeps isolated-mip generation in bounds.
        for level in 0..=a.width.ilog2() {
            let lw = (a.width >> level).max(1);
            for r in &a.sprite_rects {
                assert!(
                    (r.x >> level) < lw,
                    "sprite x={} overflows at mip {level}",
                    r.x
                );
            }
        }
    }

    #[test]
    fn every_palette_sprite_is_in_range() {
        for b in palette() {
            for s in b.sprites {
                assert!(s < sprite::COUNT, "sprite {s} out of range for {}", b.name);
            }
        }
    }
}
