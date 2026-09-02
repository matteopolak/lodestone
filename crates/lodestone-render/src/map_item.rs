//! Filled-map presentation: vanilla's `MapColor` palette, the 128×128 RGBA
//! image a map's colour bytes resolve to, and the quads that image is drawn on.
//!
//! ## What it is
//!
//! [`MapStore`](lodestone_game::maps::MapStore) keeps a map's contents as raw
//! vanilla *packed* colour bytes and deliberately refuses to resolve them —
//! "the palette is presentation and belongs to the renderer". This is that
//! renderer half: [`map_color_rgba`] is the palette, [`map_texture_rgba`] turns
//! a whole grid into an uploadable image, and [`map_quad_mesh`] is the geometry
//! that samples it.
//!
//! ## How it works
//!
//! A packed byte is `id << 2 | brightness` (vanilla's map-color packed-id
//! accessor). The high
//! six bits index the 62-entry base table below; the low two pick one of four
//! brightness modifiers, applied as an **integer** `channel * modifier / 255`
//! (vanilla's packed-RGB-scale helper). Id `0` is `MapColor.NONE`, whose
//! `calculateARGBColor` short-circuits to `0` — fully *transparent*, not black,
//! which is why an unexplored map shows the frame through it rather than a black
//! square.
//!
//! [`map_quad_mesh`] emits a single [`ModelMesh`] quad with UVs spanning the
//! whole texture, so it draws through the ordinary
//! [`ModelPipeline`](crate::ModelPipeline) with **group 1 swapped** from the block
//! atlas to the map's own texture. That is the whole reason there is no map
//! shader and no map pipeline: the model shader already samples one texture at
//! group 1 with baked absolute UVs, and it is at wgpu's 4-bind-group floor, so a
//! fifth group for a map would crash on any 4-group adapter.
//!
//! ## How to change it
//!
//! The palette is transcribed from vanilla's map-color base-colours table in the 26.2 jar, which is
//! authoritative; do not "fix" a colour against a screenshot. `MAP_COLOR_BASE`
//! is indexed by id, so a new vanilla entry appends and nothing shifts.
//!
//! Icons (`MapDecoration`) and vanilla's `map_background` frame sprite are **not**
//! drawn — the map image itself is. Both want the map-decorations atlas, which the
//! asset layer does not stitch yet.

use glam::{Mat4, Vec3};

use crate::models::{ModelMesh, ModelVertex};

/// Side length of a map's colour grid, mirroring
/// [`lodestone_game::maps::MAP_SIZE`].
pub const MAP_SIZE: u32 = 128;

/// The four brightness modifiers, indexed by vanilla's map-color brightness
/// enum's id
/// (`LOW`, `NORMAL`, `HIGH`, `LOWEST`).
///
/// The order is **not** ascending: `LOWEST` is id `3`, so a table sorted by
/// brightness would put the darkest shade where vanilla puts the lightest and
/// invert every terrain contour on the map.
pub const MAP_BRIGHTNESS: [u32; 4] = [180, 220, 255, 135];

/// Vanilla's `MapColor` base colours, indexed by id, `0xRRGGBB`.
///
/// Id `0` is `NONE` and is special-cased to transparent by [`map_color_rgba`];
/// its `0` entry here is never scaled. Transcribed verbatim from
/// vanilla's map-color base-colours table (62 entries, ids 0–61; the array vanilla
/// allocates is 64 long and the tail is `null`, resolving to `NONE`).
pub const MAP_COLOR_BASE: [u32; 62] = [
    0, 8_368_696, 16_247_203, 13_092_807, 16_711_680, 10_526_975, 10_987_431, 31_744, 16_777_215,
    10_791_096, 9_923_917, 7_368_816, 4_210_943, 9_402_184, 16_776_437, 14_188_339, 11_685_080,
    6_724_056, 15_066_419, 8_375_321, 15_892_389, 5_000_268, 10_066_329, 5_013_401, 8_339_378,
    3_361_970, 6_704_179, 6_717_235, 10_040_115, 1_644_825, 16_445_005, 6_085_589, 4_882_687,
    55_610, 8_476_209, 7_340_544, 13_742_497, 10_441_252, 9_787_244, 7_367_818, 12_223_780,
    6_780_213, 10_505_550, 3_746_083, 8_874_850, 5_725_276, 8_014_168, 4_996_700, 4_993_571,
    5_001_770, 9_321_518, 2_430_480, 12_398_641, 9_715_553, 6_035_741, 1_474_182, 3_837_580,
    5_647_422, 1_356_933, 6_579_300, 14_200_723, 8_365_974,
];

/// Resolve one packed map colour byte to RGBA8.
///
/// Vanilla's map-color packed-id-to-colour resolver: `byte >> 2` is the base id, `byte & 3` the
/// brightness. An id past the table (vanilla's `null` tail) resolves to `NONE`
/// exactly as `byIdUnsafe` does, so a malformed byte draws nothing rather than
/// indexing out of range.
#[must_use]
pub fn map_color_rgba(packed: u8) -> [u8; 4] {
    let id = usize::from(packed >> 2);
    let base = MAP_COLOR_BASE.get(id).copied().unwrap_or(0);
    if id == 0 || base == 0 {
        // Vanilla's none-color-resolution function returns literally `0`: alpha zero, so the
        // unexplored part of a map is a hole and not a black square.
        return [0, 0, 0, 0];
    }
    let modifier = MAP_BRIGHTNESS[usize::from(packed & 3)];
    let scale = |channel: u32| u8::try_from((channel * modifier / 255).min(255)).unwrap_or(255);
    [
        scale((base >> 16) & 0xFF),
        scale((base >> 8) & 0xFF),
        scale(base & 0xFF),
        255,
    ]
}

/// Resolve a whole `MAP_SIZE * MAP_SIZE` grid of packed bytes into RGBA8, row
/// major, ready for `queue.write_texture`.
///
/// A short slice is padded with transparent pixels rather than panicking: the
/// grid comes from a wire-fed store, and a truncated one should draw a partial
/// map.
#[must_use]
pub fn map_texture_rgba(colors: &[u8]) -> Vec<u8> {
    let pixels = (MAP_SIZE * MAP_SIZE) as usize;
    let mut rgba = Vec::with_capacity(pixels * 4);
    for index in 0..pixels {
        rgba.extend_from_slice(&map_color_rgba(colors.get(index).copied().unwrap_or(0)));
    }
    rgba
}

/// One textured quad, unit-sized in local `XY` and posed by `pose`, whose UVs
/// span the entire bound texture.
///
/// Local space is `x, y` in `-0.5..=0.5` at `z == 0`, so `pose` places the map's
/// centre. `V` increases downward to match the row-major image, which is what
/// puts the map's north edge at the top rather than mirroring the terrain.
///
/// `tint` is left at `255` — the palette's white slot — so the sampled texel
/// passes through unmodified. A map is already presentation-coloured; multiplying
/// it by a biome tint would green the whole picture.
#[must_use]
pub fn map_quad_mesh(pose: Mat4, light: u8) -> ModelMesh {
    // Counter-clockwise when viewed from +z, which is the front-facing winding
    // the model pipeline's back-face culling keeps.
    let corners = [
        (Vec3::new(-0.5, -0.5, 0.0), [0.0, 1.0]),
        (Vec3::new(0.5, -0.5, 0.0), [1.0, 1.0]),
        (Vec3::new(0.5, 0.5, 0.0), [1.0, 0.0]),
        (Vec3::new(-0.5, 0.5, 0.0), [0.0, 0.0]),
    ];
    let vertices = corners
        .iter()
        .map(|(local, uv)| {
            let world = pose.transform_point3(*local);
            ModelVertex {
                position: world.to_array(),
                uv: *uv,
                ao: 1.0,
                light,
                tint: 255,
                anim: 0,
                cutout_bypass: 0,
                tint_rgb_override: [0, 0, 0, 0],
            }
        })
        .collect();
    ModelMesh {
        vertices,
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The palette against vanilla's map-color base-colours table read as a record definition, at both
    /// ends of the brightness range and on the entry a wrong brightness order
    /// would flip.
    ///
    /// `GRASS` is id 1 (`0x7FB238` = 8368696). Packed `1 << 2 | 2` is `HIGH`
    /// (modifier 255, i.e. unchanged); `1 << 2 | 3` is `LOWEST` (135), which is
    /// the entry that lands on `NORMAL`'s 220 if the table is sorted by
    /// brightness instead of by id.
    #[test]
    fn the_palette_matches_the_jar() {
        assert_eq!(map_color_rgba(0b0000_0110), [0x7F, 0xB2, 0x38, 255]);
        assert_eq!(
            map_color_rgba(0b0000_0111),
            [
                u8::try_from(0x7F * 135 / 255).unwrap(),
                u8::try_from(0xB2 * 135 / 255).unwrap(),
                u8::try_from(0x38 * 135 / 255).unwrap(),
                255
            ]
        );
        // `LOW` (180) must be darker than `NORMAL` (220) must be darker than
        // `HIGH` (255) — the contour ordering, stated as values not as a sign.
        let low = map_color_rgba(0b0000_0100)[1];
        let normal = map_color_rgba(0b0000_0101)[1];
        let high = map_color_rgba(0b0000_0110)[1];
        assert_eq!(
            (low, normal, high),
            (
                u8::try_from(0xB2 * 180 / 255).unwrap(),
                u8::try_from(0xB2 * 220 / 255).unwrap(),
                0xB2
            )
        );
    }

    /// `MapColor.NONE` is transparent, not black. An unexplored map must be a
    /// hole: filling it with opaque black would hide whatever the map is drawn
    /// over and look like a rendering failure.
    #[test]
    fn unexplored_is_transparent() {
        for brightness in 0..4u8 {
            assert_eq!(map_color_rgba(brightness), [0, 0, 0, 0]);
        }
        let rgba = map_texture_rgba(&[]);
        assert_eq!(rgba.len(), (MAP_SIZE * MAP_SIZE) as usize * 4);
        assert!(rgba.iter().all(|byte| *byte == 0));
    }

    /// An id past vanilla's populated range resolves to `NONE` rather than
    /// panicking — the array vanilla allocates is 64 long and only 62 entries
    /// are non-null.
    #[test]
    fn an_id_past_the_table_is_none() {
        assert_eq!(map_color_rgba(63 << 2), [0, 0, 0, 0]);
        assert_eq!(map_color_rgba(u8::MAX), [0, 0, 0, 0]);
    }

    /// The image is row-major and its `V` grows downward, so grid row 0 lands on
    /// the quad's **top** edge. Getting this upside down mirrors the terrain
    /// north-for-south, which reads as plausible on an unfamiliar map.
    #[test]
    fn the_quad_puts_row_zero_at_the_top() {
        let mesh = map_quad_mesh(Mat4::IDENTITY, 15);
        let top = mesh
            .vertices
            .iter()
            .filter(|v| v.position[1] > 0.0)
            .collect::<Vec<_>>();
        assert_eq!(top.len(), 2);
        assert!(top.iter().all(|v| v.uv[1] == 0.0));
        assert!(mesh.vertices.iter().all(|v| v.tint == 255));
        assert_eq!(mesh.quad_count(), 1);
    }
}
