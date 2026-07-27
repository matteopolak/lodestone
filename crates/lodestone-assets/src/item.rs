//! `builtin/generated` item-model geometry: the 2D-sprite-extruded-to-3D path.
//!
//! Most item models resolve through the ordinary block-model chain
//! ([`crate::model`]): they set a `parent`, `textures`, and `display` transforms,
//! and either carry `elements` (a normal 3D model, e.g. blocks held in hand) or
//! inherit `builtin/generated`. Only the `builtin/generated` case needs special
//! geometry — vanilla builds a mesh from each layer texture's **alpha outline**:
//! a front and back quad at depth `z ∈ [7.5, 8.5]` texels, plus a one-pixel-thick
//! side wall on every opaque/transparent boundary edge so the flat sprite gains a
//! visible thickness. `builtin/entity` items (chests, shulker boxes, banners…)
//! are drawn by a dedicated entity renderer and carry no bakeable geometry here.
//!
//! This is faithful to `net/minecraft/client/resources/model/cuboid/
//! ItemModelGenerator` in the decompiled 26.2 client. Output is GPU-free CPU data
//! ([`ItemQuad`]); positions are world units (texels / 16).

use crate::model::Direction;
use crate::texture::Image;

/// The front/back plane depths, in texels (vanilla `MIN_Z`/`MAX_Z`).
const MIN_Z: f32 = 7.5;
const MAX_Z: f32 = 8.5;
/// UV inset applied to side-wall strips (vanilla `UV_SHRINK`).
const UV_SHRINK: f32 = 0.1;
/// The five texture layers a generated item may stack (`layer0`..`layer4`).
pub const LAYER_NAMES: [&str; 5] = ["layer0", "layer1", "layer2", "layer3", "layer4"];

/// One baked quad of a generated item model. Positions are world units; UVs are
/// normalised to `[0, 1]` against the layer sprite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemQuad {
    /// The four corner positions, world units.
    pub positions: [[f32; 3]; 4],
    /// The four corner UVs, normalised to `[0, 1]`.
    pub uvs: [[f32; 2]; 4],
    /// The outward face direction (front `South`, back `North`, side walls
    /// carry the vanilla side label).
    pub direction: Direction,
    /// Which `layerN` texture this quad came from.
    pub layer: u8,
}

/// A pixel is transparent when its alpha is zero (matching vanilla's
/// `SpriteContents.isTransparent`), or when it lies outside the sprite.
fn is_transparent(img: &Image, x: i64, y: i64) -> bool {
    if x < 0 || y < 0 || x >= img.width as i64 || y >= img.height as i64 {
        return true;
    }
    let idx = ((y as u32 * img.width + x as u32) * 4 + 3) as usize;
    img.rgba[idx] == 0
}

/// Bakes each layer sprite into extruded item geometry. Never panics on odd
/// input — an all-transparent sprite still yields its front/back quads, and
/// mismatched sizes are handled per layer.
pub fn bake_item_generated(layers: &[&Image]) -> Vec<ItemQuad> {
    let mut out = Vec::new();
    for (li, img) in layers.iter().enumerate() {
        let layer = li as u8;
        // Front (SOUTH) at MAX_Z and back (NORTH) at MIN_Z, full-sprite UVs.
        out.push(front_face(layer));
        out.push(back_face(layer));
        if img.width == 0 || img.height == 0 {
            continue;
        }
        bake_side_walls(img, layer, &mut out);
    }
    out
}

fn front_face(layer: u8) -> ItemQuad {
    let z = MAX_Z / 16.0;
    ItemQuad {
        // CCW seen from +Z, world y up. UV v=0 is the top texture row (y=1).
        positions: [[0.0, 1.0, z], [0.0, 0.0, z], [1.0, 0.0, z], [1.0, 1.0, z]],
        uvs: [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
        direction: Direction::South,
        layer,
    }
}

fn back_face(layer: u8) -> ItemQuad {
    let z = MIN_Z / 16.0;
    ItemQuad {
        positions: [[0.0, 1.0, z], [1.0, 1.0, z], [1.0, 0.0, z], [0.0, 0.0, z]],
        // NORTH_FACE_UVS mirrors U (16,0,0,16).
        uvs: [[1.0, 0.0], [0.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
        direction: Direction::North,
        layer,
    }
}

/// Vanilla's four side-wall directions over the sprite grid. Each carries the
/// neighbour it probes for transparency and the face label vanilla assigns.
#[derive(Clone, Copy)]
enum Side {
    Up,
    Down,
    Left,
    Right,
}

impl Side {
    /// The neighbour probed for transparency: `(x - stepX, y - stepY)` in
    /// vanilla, using the mapped 3D direction's step.
    fn neighbour(self, x: i64, y: i64) -> (i64, i64) {
        match self {
            Side::Up => (x, y - 1),
            Side::Down => (x, y + 1),
            Side::Left => (x - 1, y),
            Side::Right => (x + 1, y),
        }
    }

    fn direction(self) -> Direction {
        // Vanilla: UP→UP, DOWN→DOWN, LEFT→EAST, RIGHT→WEST.
        match self {
            Side::Up => Direction::Up,
            Side::Down => Direction::Down,
            Side::Left => Direction::East,
            Side::Right => Direction::West,
        }
    }

    fn is_horizontal(self) -> bool {
        matches!(self, Side::Up | Side::Down)
    }
}

fn bake_side_walls(img: &Image, layer: u8, out: &mut Vec<ItemQuad>) {
    let w = img.width;
    let h = img.height;
    let x_scale = 16.0 / w as f32;
    let y_scale = 16.0 / h as f32;
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            if is_transparent(img, x, y) {
                continue;
            }
            for side in [Side::Up, Side::Down, Side::Left, Side::Right] {
                let (nx, ny) = side.neighbour(x, y);
                if is_transparent(img, nx, ny) {
                    out.push(side_wall(side, x as f32, y as f32, x_scale, y_scale, layer));
                }
            }
        }
    }
}

/// Builds one side-wall quad, faithful to `ItemModelGenerator.bakeSideFaces`.
fn side_wall(side: Side, x: f32, y: f32, x_scale: f32, y_scale: f32, layer: u8) -> ItemQuad {
    // UV strip (texel space, 0..16), inset by UV_SHRINK.
    let u0 = x + UV_SHRINK;
    let u1 = x + 1.0 - UV_SHRINK;
    let (v0, v1) = if side.is_horizontal() {
        (y + UV_SHRINK, y + 1.0 - UV_SHRINK)
    } else {
        (y + 1.0 - UV_SHRINK, y + UV_SHRINK)
    };

    // Endpoints in pixel space, per the vanilla switch.
    let (mut sx, mut sy, mut ex, mut ey) = (x, y, x, y);
    match side {
        Side::Up => ex += 1.0,
        Side::Down => {
            ex += 1.0;
            sy += 1.0;
            ey += 1.0;
        }
        Side::Left => ey += 1.0,
        Side::Right => {
            sx += 1.0;
            ex += 1.0;
            ey += 1.0;
        }
    }
    sx *= x_scale;
    ex *= x_scale;
    sy *= y_scale;
    ey *= y_scale;
    sy = 16.0 - sy;
    ey = 16.0 - ey;

    // from/to box (flat in one axis), z spanning the extrusion depth.
    let (from, to) = match side {
        Side::Up => ([sx, sy, MIN_Z], [ex, sy, MAX_Z]),
        Side::Down => ([sx, ey, MIN_Z], [ex, ey, MAX_Z]),
        Side::Left => ([sx, sy, MIN_Z], [sx, ey, MAX_Z]),
        Side::Right => ([ex, sy, MIN_Z], [ex, ey, MAX_Z]),
    };

    let positions = wall_corners(from, to);
    let uvs = [
        [u0 * x_scale / 16.0, v0 * y_scale / 16.0],
        [u1 * x_scale / 16.0, v0 * y_scale / 16.0],
        [u1 * x_scale / 16.0, v1 * y_scale / 16.0],
        [u0 * x_scale / 16.0, v1 * y_scale / 16.0],
    ];
    ItemQuad {
        positions,
        uvs,
        direction: side.direction(),
        layer,
    }
}

/// Four world-space corners of an axis-aligned wall spanning the two axes in
/// which `from` and `to` differ (one of x/y, plus z).
fn wall_corners(from: [f32; 3], to: [f32; 3]) -> [[f32; 3]; 4] {
    let f = [from[0] / 16.0, from[1] / 16.0, from[2] / 16.0];
    let t = [to[0] / 16.0, to[1] / 16.0, to[2] / 16.0];
    if (f[0] - t[0]).abs() > 1e-9 {
        // Wall spans x and z at constant y.
        let y = f[1];
        [
            [f[0], y, f[2]],
            [t[0], y, f[2]],
            [t[0], y, t[2]],
            [f[0], y, t[2]],
        ]
    } else {
        // Wall spans y and z at constant x.
        let x = f[0];
        [
            [x, f[1], f[2]],
            [x, t[1], f[2]],
            [x, t[1], t[2]],
            [x, f[1], t[2]],
        ]
    }
}
