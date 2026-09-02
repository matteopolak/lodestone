//! Packed chunk vertex format.
//!
//! A loaded world is millions of vertices, so bytes-per-vertex directly sets
//! both VRAM footprint and vertex-fetch bandwidth. The naive
//! `struct { pos:[f32;3], uv:[f32;2], normal:[f32;3], colour:[f32;4] }` is
//! **48 bytes**. Everything a chunk vertex actually needs fits in far fewer bits
//! because the values live on small, known grids:
//!
//! * Position lands on a per-section sub-block grid, not arbitrary floats.
//! * The normal is one of 6 face directions.
//! * Light and AO are tiny integers.
//! * The texture is an atlas sprite index plus a small in-sprite tile coord.
//!
//! # Bit layout (3 × u32 = 12 bytes)
//!
//! Positions use **6 bits/axis** giving the range `0..=63`. A 16³ section needs
//! `0..=16` for greedy quad corners (a quad can span the whole section, so the
//! far edge is at coordinate 16), so 5 bits (`0..=31`) already suffices; the 6th
//! bit is deliberate headroom for future half-block / fluid-level geometry.
//!
//! AO and light are stored as **8-bit brightness bytes** (`0..=255`), not the
//! raw `0..=15` lightmap / `0..=3` occlusion integers. That width is what lets
//! the mesher store *smooth* (per-corner, 4-sample-averaged) lighting — see
//! [`mesh`](crate::mesh) — whose blended values are fractional; a 2-bit AO field
//! would band visibly on curved geometry. This directly cost us 4 bytes/vertex
//! (8→12) and was the measured trade for vanilla-parity smooth lighting.
//!
//! ```text
//! word0:
//!   bits  0..6   x            (0..=63)
//!   bits  6..12  y            (0..=63)
//!   bits 12..18  z            (0..=63)
//!   bits 18..21  normal       (0..=5, a Face index)
//!   bits 21..32  (reserved, 0)
//!
//! word1:
//!   bits  0..11  sprite       (0..=2047)
//!   bits 11..16  u            (0..=16 tiles)
//!   bits 16..21  v            (0..=16 tiles)
//!   bits 21..32  (reserved, 0)
//!
//! word2:
//!   bits  0..8   ao           (0..=255 brightness)
//!   bits  8..16  sky_light    (0..=255 brightness)
//!   bits 16..24  block_light  (0..=255 brightness)
//!   bits 24..32  (reserved, 0 — 8 bits for e.g. biome tint index)
//! ```
//!
//! `u`/`v` are in *tile* units (whole-sprite repeats), 5 bits each, so a greedy
//! quad up to 16×16 tiles is representable and the shader multiplies by the
//! sprite's UV size to get atlas coordinates. That keeps texture wrapping exact
//! for merged quads instead of stretching one sprite across the merge.
//!
//! At **12 bytes/vertex** this is a **4×** reduction versus the 48-byte
//! baseline; see [`vram_bytes`] and the crate report for the render-distance-32
//! projection.

use bytemuck::{Pod, Zeroable};

use crate::section::Face;

/// Bits allotted to each position axis (`0..=63`).
pub const POS_BITS: u32 = 6;
/// Maximum encodable position coordinate.
pub const POS_MAX: u32 = (1 << POS_BITS) - 1;
/// Maximum encodable tile coordinate for `u`/`v` (5-bit field).
pub const UV_MAX: u32 = 31;
/// Maximum encodable sprite index (11-bit field), matching a 2048-entry atlas.
pub const SPRITE_MAX: u32 = 2047;
/// Maximum ambient-occlusion brightness byte (8-bit field).
pub const AO_MAX: u32 = 255;
/// Maximum light brightness byte, block or sky (8-bit field).
pub const LIGHT_MAX: u32 = 255;

/// A packed chunk vertex: three little-endian `u32` words, 12 bytes total.
///
/// `#[repr(C)]` + `Pod` so slices upload directly as GPU vertex data.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct PackedVertex {
    /// Packed words; see the module docs for the layout.
    pub words: [u32; 3],
}

/// The unpacked fields of a [`PackedVertex`], for construction and testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexFields {
    /// Section-local position, each axis `0..=63`.
    pub pos: [u32; 3],
    /// Face direction of the vertex's quad.
    pub normal: Face,
    /// Ambient-occlusion brightness `0..=255` (255 = unoccluded). This is a
    /// smooth, 4-sample-averaged value from the mesher, not a `0..=3` level.
    pub ao: u8,
    /// Sky-light brightness `0..=255`, smoothed across the corner's neighbours.
    pub sky_light: u8,
    /// Block-light brightness `0..=255`, smoothed across the corner's neighbours.
    pub block_light: u8,
    /// Atlas sprite index `0..=2047`.
    pub sprite: u16,
    /// Tile-space U coordinate `0..=31`.
    pub u: u8,
    /// Tile-space V coordinate `0..=31`.
    pub v: u8,
}

const fn field(value: u32, shift: u32, bits: u32) -> u32 {
    (value & ((1 << bits) - 1)) << shift
}

const fn get(word: u32, shift: u32, bits: u32) -> u32 {
    (word >> shift) & ((1 << bits) - 1)
}

impl PackedVertex {
    /// Pack fields into the three-word representation.
    ///
    /// Each field is masked to its bit width. Debug builds assert the inputs are
    /// in range so producers catch overflow during development; release builds
    /// silently mask (the geometry stays well-defined, just wrapped).
    #[must_use]
    pub fn pack(f: VertexFields) -> Self {
        debug_assert!(f.pos[0] <= POS_MAX && f.pos[1] <= POS_MAX && f.pos[2] <= POS_MAX);
        debug_assert!(f.sprite as u32 <= SPRITE_MAX);
        debug_assert!(f.u as u32 <= UV_MAX && f.v as u32 <= UV_MAX);

        let normal = f.normal.index() as u32;
        let word0 = field(f.pos[0], 0, 6)
            | field(f.pos[1], 6, 6)
            | field(f.pos[2], 12, 6)
            | field(normal, 18, 3);
        let word1 =
            field(f.sprite as u32, 0, 11) | field(f.u as u32, 11, 5) | field(f.v as u32, 16, 5);
        let word2 = field(f.ao as u32, 0, 8)
            | field(f.sky_light as u32, 8, 8)
            | field(f.block_light as u32, 16, 8);
        Self {
            words: [word0, word1, word2],
        }
    }

    /// Unpack the three-word representation back into fields.
    #[must_use]
    pub fn unpack(self) -> VertexFields {
        let [w0, w1, w2] = self.words;
        let normal = match get(w0, 18, 3) {
            0 => Face::NegX,
            1 => Face::PosX,
            2 => Face::NegY,
            3 => Face::PosY,
            4 => Face::NegZ,
            _ => Face::PosZ,
        };
        VertexFields {
            pos: [get(w0, 0, 6), get(w0, 6, 6), get(w0, 12, 6)],
            normal,
            ao: get(w2, 0, 8) as u8,
            sky_light: get(w2, 8, 8) as u8,
            block_light: get(w2, 16, 8) as u8,
            sprite: get(w1, 0, 11) as u16,
            u: get(w1, 11, 5) as u8,
            v: get(w1, 16, 5) as u8,
        }
    }

    /// The `wgpu` vertex buffer layout for this format: one `Uint32x3` attribute.
    #[must_use]
    pub fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 1] = [wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint32x3,
            offset: 0,
            shader_location: 0,
        }];
        wgpu::VertexBufferLayout {
            array_stride: core::mem::size_of::<PackedVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRS,
        }
    }
}

/// Bytes per packed vertex (compile-time constant, asserted to be 12).
pub const BYTES_PER_VERTEX: usize = core::mem::size_of::<PackedVertex>();

/// Bytes per index (`u32` index buffers).
pub const BYTES_PER_INDEX: usize = core::mem::size_of::<u32>();

/// Estimated total VRAM in bytes for `quad_count` quads, counting both the
/// packed vertices (4 per quad) and the `u32` index buffer (6 per quad).
///
/// This is the honest per-quad cost of an indexed quad mesh, used for the
/// render-distance projection in the crate report.
#[must_use]
pub const fn vram_bytes(quad_count: usize) -> usize {
    let vertices = quad_count * 4 * BYTES_PER_VERTEX;
    let indices = quad_count * 6 * BYTES_PER_INDEX;
    vertices + indices
}

/// The same projection using the 48-byte naive vertex, for comparison.
#[must_use]
pub const fn naive_vram_bytes(quad_count: usize) -> usize {
    let vertices = quad_count * 4 * 48;
    let indices = quad_count * 6 * BYTES_PER_INDEX;
    vertices + indices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_is_twelve_bytes() {
        assert_eq!(BYTES_PER_VERTEX, 12);
        assert_eq!(core::mem::align_of::<PackedVertex>(), 4);
    }

    #[allow(clippy::too_many_arguments)]
    fn sample(
        pos: [u32; 3],
        normal: Face,
        ao: u8,
        sky: u8,
        block: u8,
        sprite: u16,
        u: u8,
        v: u8,
    ) -> VertexFields {
        VertexFields {
            pos,
            normal,
            ao,
            sky_light: sky,
            block_light: block,
            sprite,
            u,
            v,
        }
    }

    #[test]
    fn round_trips_all_faces() {
        for normal in Face::ALL {
            let f = sample([1, 2, 3], normal, 1, 5, 6, 42, 0, 0);
            assert_eq!(PackedVertex::pack(f).unpack(), f, "face {normal:?}");
        }
    }

    #[test]
    fn round_trips_field_maxima() {
        let f = sample(
            [POS_MAX, POS_MAX, POS_MAX],
            Face::PosZ,
            AO_MAX as u8,
            LIGHT_MAX as u8,
            LIGHT_MAX as u8,
            SPRITE_MAX as u16,
            UV_MAX as u8,
            UV_MAX as u8,
        );
        assert_eq!(PackedVertex::pack(f).unpack(), f);
    }

    #[test]
    fn round_trips_field_minima() {
        let f = sample([0, 0, 0], Face::NegX, 0, 0, 0, 0, 0, 0);
        let packed = PackedVertex::pack(f);
        assert_eq!(packed.words, [0, 0, 0]);
        assert_eq!(packed.unpack(), f);
    }

    #[test]
    fn fields_do_not_bleed_into_each_other() {
        // Set exactly one field high and confirm neighbours stay zero.
        let base = sample([0, 0, 0], Face::NegX, 0, 0, 0, 0, 0, 0);
        let mut f = base;
        f.pos[0] = POS_MAX;
        let u = PackedVertex::pack(f).unpack();
        assert_eq!(u.pos, [POS_MAX, 0, 0]);
        assert_eq!(u.sky_light, 0);

        let mut f = base;
        f.block_light = LIGHT_MAX as u8;
        let u = PackedVertex::pack(f).unpack();
        assert_eq!(u.block_light, LIGHT_MAX as u8);
        assert_eq!(u.sky_light, 0);
        assert_eq!(u.pos, [0, 0, 0]);

        let mut f = base;
        f.v = UV_MAX as u8;
        let u = PackedVertex::pack(f).unpack();
        assert_eq!(u.v, UV_MAX as u8);
        assert_eq!(u.u, 0);
        assert_eq!(u.sprite, 0);
    }

    #[test]
    fn reserved_bits_stay_zero() {
        let f = sample(
            [POS_MAX, POS_MAX, POS_MAX],
            Face::PosZ,
            AO_MAX as u8,
            LIGHT_MAX as u8,
            LIGHT_MAX as u8,
            SPRITE_MAX as u16,
            UV_MAX as u8,
            UV_MAX as u8,
        );
        let [w0, w1, w2] = PackedVertex::pack(f).words;
        assert_eq!(w0 >> 21, 0, "word0 bits 21..32 reserved");
        assert_eq!(w1 >> 21, 0, "word1 bits 21..32 reserved");
        assert_eq!(w2 >> 24, 0, "word2 bits 24..32 reserved");
    }
}
