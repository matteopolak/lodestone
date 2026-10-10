//! Filled-map presentation: vanilla's map-colour palette, the 128×128 RGBA
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
//! (vanilla's packed-RGB-scale helper). Id `0` is the none colour, whose
//! ARGB calculation short-circuits to `0` — fully *transparent*, not black,
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
//! ## Decorations
//!
//! A decoration (player arrow, banner, structure icon) is a second textured quad
//! per icon, sampling the stitched decoration sheet
//! ([`lodestone_assets::map_decoration_atlas`]) at group 1 in place of the map
//! texture. [`MAP_DECORATION_TYPES`] maps a decoration type key to its sprite id
//! and whether an item frame shows it; the sprites themselves come from the pack.
//! [`map_decoration_mesh`] places each in the map's pixel space exactly as the
//! map image is: wire units are half pixels, the sprite is 8x8 pixels centred a
//! half pixel up-left of the point, rotation is sixteenths of a turn clockwise.
//! The vanilla `map_background` frame sprite is still not drawn.

use glam::{Mat4, Vec3};

use crate::models::{ModelMesh, ModelVertex};

/// One decoration type: registry key path, sprite id (a bare path in the
/// `minecraft` namespace, as the decoration sheet names it), and whether an
/// item frame draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapDecorationType {
    /// Path of the `minecraft:map_decoration_type` key the wire resolves to.
    pub key: &'static str,
    /// Sprite id on the decoration sheet, in the `minecraft` namespace.
    pub sprite: &'static str,
    /// Whether the icon is drawn when the map hangs in an item frame.
    pub show_on_item_frame: bool,
}

const fn decoration(key: &'static str, sprite: &'static str, show_on_item_frame: bool) -> MapDecorationType {
    MapDecorationType { key, sprite, show_on_item_frame }
}

/// Every 26.3 decoration type, in registry order.
pub const MAP_DECORATION_TYPES: [MapDecorationType; 40] = [
    decoration("player", "player", false),
    decoration("frame", "frame", true),
    decoration("red_marker", "red_marker", false),
    decoration("blue_marker", "blue_marker", false),
    decoration("target_x", "target_x", true),
    decoration("target_point", "target_point", true),
    decoration("player_off_map", "player_off_map", false),
    decoration("player_off_limits", "player_off_limits", false),
    decoration("mansion", "woodland_mansion", true),
    decoration("monument", "ocean_monument", true),
    decoration("banner_white", "white_banner", true),
    decoration("banner_orange", "orange_banner", true),
    decoration("banner_magenta", "magenta_banner", true),
    decoration("banner_light_blue", "light_blue_banner", true),
    decoration("banner_yellow", "yellow_banner", true),
    decoration("banner_lime", "lime_banner", true),
    decoration("banner_pink", "pink_banner", true),
    decoration("banner_gray", "gray_banner", true),
    decoration("banner_light_gray", "light_gray_banner", true),
    decoration("banner_cyan", "cyan_banner", true),
    decoration("banner_purple", "purple_banner", true),
    decoration("banner_blue", "blue_banner", true),
    decoration("banner_brown", "brown_banner", true),
    decoration("banner_green", "green_banner", true),
    decoration("banner_red", "red_banner", true),
    decoration("banner_black", "black_banner", true),
    decoration("red_x", "red_x", true),
    decoration("village_desert", "desert_village", true),
    decoration("village_plains", "plains_village", true),
    decoration("village_savanna", "savanna_village", true),
    decoration("village_snowy", "snowy_village", true),
    decoration("village_taiga", "taiga_village", true),
    decoration("jungle_temple", "jungle_temple", true),
    decoration("swamp_hut", "swamp_hut", true),
    decoration("trial_chambers", "trial_chambers", true),
    decoration("abandoned_camp", "abandoned_camp", true),
    decoration("ancient_city", "ancient_city", true),
    decoration("desert_pyramid", "desert_pyramid", true),
    decoration("mineshaft", "mineshaft", true),
    decoration("ocean_ruin_warm", "warm_ocean_ruins", true),
];

/// The decoration type whose registry key path is `key`.
#[must_use]
pub fn map_decoration_type(key: &str) -> Option<&'static MapDecorationType> {
    MAP_DECORATION_TYPES.iter().find(|ty| ty.key == key)
}

/// Where one decoration sprite goes on a map and which sheet region it samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapDecorationPlacement {
    /// Top-left UV of the sprite on the decoration sheet.
    pub uv_min: [f32; 2],
    /// Bottom-right UV of the sprite on the decoration sheet.
    pub uv_max: [f32; 2],
    /// Wire x: half map pixels from the map's centre.
    pub x: i8,
    /// Wire y: half map pixels from the map's centre.
    pub y: i8,
    /// Sixteenths of a clockwise turn, `0..=15`.
    pub rotation: u8,
}

/// Distance in front of the map picture of the first decoration, in map pixels,
/// and the extra distance for each later one. Large enough to survive the
/// forward depth buffer at several blocks; the reference's thousandth-of-a-pixel
/// steps do not.
const DECORATION_LIFT_PIXELS: f32 = 0.5;
const DECORATION_STACK_PIXELS: f32 = 0.1;

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

/// Vanilla's map base colours, indexed by id, `0xRRGGBB`.
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

/// One packed map-colour byte from the fixed presentation palette.
///
/// The high six bits select one of the 62 populated base-colour entries and
/// the low two bits select its brightness. A byte whose base id is 62 or 63
/// is outside the populated palette and is rejected before colour arithmetic
/// runs. Keeping this value typed prevents an arbitrary byte from being passed
/// to the palette resolver as if it named a real colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PackedMapColour(u8);

impl PackedMapColour {
    /// Validates a packed map-colour byte against the populated palette.
    #[must_use]
    pub const fn new(raw: u8) -> Option<Self> {
        if (raw >> 2) < MAP_COLOR_BASE.len() as u8 {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// The packed byte used by the map grid.
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }

    /// The transparent colour used when a malformed wire byte is discarded.
    pub const NONE: Self = Self(0);
}

/// Resolve one packed map colour byte to RGBA8.
///
/// The packed-id-to-colour resolver: `byte >> 2` is the base id, `byte & 3` the
/// brightness. [`map_texture_rgba`] turns an id past the populated table into
/// `NONE` at the grid boundary, so malformed grid data draws nothing rather
/// than indexing out of range. This function only accepts a validated
/// [`PackedMapColour`], so its table access is total.
#[must_use]
pub fn map_color_rgba(packed: PackedMapColour) -> [u8; 4] {
    let id = usize::from(packed.raw() >> 2);
    let base = MAP_COLOR_BASE[id];
    if id == 0 {
        // Vanilla's none-color-resolution function returns literally `0`: alpha zero, so the
        // unexplored part of a map is a hole and not a black square.
        return [0, 0, 0, 0];
    }
    let modifier = MAP_BRIGHTNESS[usize::from(packed.raw() & 3)];
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
        let packed = colors
            .get(index)
            .copied()
            .and_then(PackedMapColour::new)
            .unwrap_or(PackedMapColour::NONE);
        rgba.extend_from_slice(&map_color_rgba(packed));
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

/// The quads for `placements`, on the plane of a [`map_quad_mesh`] posed by
/// `pose`. `None` when there is nothing to draw.
///
/// Each sprite spans 8x8 map pixels. Its centre sits at the wire point
/// (`x / 2 + 64`, `y / 2 + 64`) offset half a pixel left and down in map space,
/// rotated clockwise about that point, and its top texel row faces the lower map
/// edge: the sprite is vertically flipped against the map image, as the
/// reference's vertex order makes it. Later placements sit slightly further
/// out so overlapping icons keep their order.
#[must_use]
pub fn map_decoration_mesh(pose: Mat4, light: u8, placements: &[MapDecorationPlacement]) -> Option<ModelMesh> {
    if placements.is_empty() {
        return None;
    }
    let mut vertices = Vec::with_capacity(placements.len() * 4);
    let mut indices = Vec::with_capacity(placements.len() * 6);
    for (n, placement) in placements.iter().enumerate() {
        let angle = f32::from(placement.rotation) * std::f32::consts::TAU / 16.0;
        let (sin, cos) = angle.sin_cos();
        let centre = [f32::from(placement.x) / 2.0 + 64.0, f32::from(placement.y) / 2.0 + 64.0];
        let lift = (DECORATION_LIFT_PIXELS + DECORATION_STACK_PIXELS * n as f32) / MAP_SIZE as f32;
        // Corners in map-pixel space (x right, y down) before the half-pixel
        // shift, paired with the sprite corner each samples.
        let corners = [
            ([-4.0, 4.0], [placement.uv_min[0], placement.uv_min[1]]),
            ([4.0, 4.0], [placement.uv_max[0], placement.uv_min[1]]),
            ([4.0, -4.0], [placement.uv_max[0], placement.uv_max[1]]),
            ([-4.0, -4.0], [placement.uv_min[0], placement.uv_max[1]]),
        ];
        let base = u32::try_from(vertices.len()).expect("a decoration mesh stays far below u32::MAX vertices");
        for (corner, uv) in corners {
            let (px, py) = (corner[0] - 0.5, corner[1] + 0.5);
            let (rx, ry) = (px * cos - py * sin, px * sin + py * cos);
            let map = [centre[0] + rx, centre[1] + ry];
            let local = Vec3::new(map[0] / MAP_SIZE as f32 - 0.5, 0.5 - map[1] / MAP_SIZE as f32, lift);
            vertices.push(ModelVertex {
                position: pose.transform_point3(local).to_array(),
                uv,
                ao: 1.0,
                light,
                tint: 255,
                anim: 0,
                cutout_bypass: 0,
                tint_rgb_override: [0, 0, 0, 0],
            });
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Some(ModelMesh { vertices, indices })
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
        let high = PackedMapColour::new(0b0000_0110).expect("grass high is populated");
        assert_eq!(map_color_rgba(high), [0x7F, 0xB2, 0x38, 255]);
        assert_eq!(
            map_color_rgba(PackedMapColour::new(0b0000_0111).expect("grass lowest is populated")),
            [
                u8::try_from(0x7F * 135 / 255).unwrap(),
                u8::try_from(0xB2 * 135 / 255).unwrap(),
                u8::try_from(0x38 * 135 / 255).unwrap(),
                255
            ]
        );
        // `LOW` (180) must be darker than `NORMAL` (220) must be darker than
        // `HIGH` (255) — the contour ordering, stated as values not as a sign.
        let low = map_color_rgba(PackedMapColour::new(0b0000_0100).unwrap())[1];
        let normal = map_color_rgba(PackedMapColour::new(0b0000_0101).unwrap())[1];
        let high = map_color_rgba(PackedMapColour::new(0b0000_0110).unwrap())[1];
        assert_eq!(
            (low, normal, high),
            (
                u8::try_from(0xB2 * 180 / 255).unwrap(),
                u8::try_from(0xB2 * 220 / 255).unwrap(),
                0xB2
            )
        );
    }

    /// The none map colour is transparent, not black. An unexplored map must be a
    /// hole: filling it with opaque black would hide whatever the map is drawn
    /// over and look like a rendering failure.
    #[test]
    fn unexplored_is_transparent() {
        for brightness in 0..4u8 {
            assert_eq!(
                map_color_rgba(PackedMapColour::new(brightness).unwrap()),
                [0, 0, 0, 0]
            );
        }
        let rgba = map_texture_rgba(&[]);
        assert_eq!(rgba.len(), (MAP_SIZE * MAP_SIZE) as usize * 4);
        assert!(rgba.iter().all(|byte| *byte == 0));
    }

    /// A base id past the populated range is rejected rather than smuggled
    /// into the resolver. The texture boundary still fails closed to the
    /// transparent entry, preserving the old malformed-grid behaviour.
    #[test]
    fn packed_colour_rejects_ids_past_the_table() {
        assert_eq!(PackedMapColour::new(62 << 2), None);
        assert_eq!(PackedMapColour::new(u8::MAX), None);
        assert_eq!(
            map_texture_rgba(&[62 << 2]),
            vec![0; (MAP_SIZE * MAP_SIZE) as usize * 4]
        );
    }

    /// The boundary keeps the two packed fields together: a real base id with
    /// the highest brightness remains distinct from the neighbouring base id.
    #[test]
    fn packed_colour_accepts_a_populated_high_brightness_value() {
        let colour = PackedMapColour::new((61 << 2) | 2).expect("last palette entry is valid");
        assert_eq!(colour.raw(), (61 << 2) | 2);
        assert_eq!(map_color_rgba(colour), [0x7F, 0xA7, 0x96, 255]);
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

    fn placement(x: i8, y: i8, rotation: u8) -> MapDecorationPlacement {
        MapDecorationPlacement { uv_min: [0.0, 0.0], uv_max: [1.0, 1.0], x, y, rotation }
    }

    fn corner(mesh: &ModelMesh, index: usize) -> ([f32; 3], [f32; 2]) {
        (mesh.vertices[index].position, mesh.vertices[index].uv)
    }

    /// A centred upright icon: the sprite spans 8 map pixels, centred half a
    /// pixel up-left of the map centre, so on the unit quad (128 pixels wide)
    /// its corners sit at `59.5/128 - 0.5` and `67.5/128 - 0.5` across and
    /// `0.5 - 60.5/128` and `0.5 - 68.5/128` down, and the sprite's top row
    /// is at the lower edge.
    #[test]
    fn an_upright_decoration_spans_eight_pixels_at_the_reference_offset() {
        let mesh = map_decoration_mesh(Mat4::IDENTITY, 15, &[placement(0, 0, 0)]).unwrap();
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1.0e-6);
        let (left, right) = (-0.035_156_25, 0.027_343_75);
        let (low, high) = (-0.035_156_25, 0.027_343_75);
        let lift = 0.5 / 128.0;
        let (p, uv) = corner(&mesh, 0);
        assert!(near(p, [left, low, lift]) && uv == [0.0, 0.0], "{p:?} {uv:?}");
        let (p, uv) = corner(&mesh, 1);
        assert!(near(p, [right, low, lift]) && uv == [1.0, 0.0], "{p:?} {uv:?}");
        let (p, uv) = corner(&mesh, 2);
        assert!(near(p, [right, high, lift]) && uv == [1.0, 1.0], "{p:?} {uv:?}");
        let (p, uv) = corner(&mesh, 3);
        assert!(near(p, [left, high, lift]) && uv == [0.0, 1.0], "{p:?} {uv:?}");
        assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3]);
    }

    /// Wire units are half pixels, and rotation 4 is a quarter turn clockwise
    /// on the picture: the sprite's lower-right corner `(3.5, 4.5)` pixels from
    /// the point lands at `(-4.5, 3.5)`.
    #[test]
    fn position_is_half_pixels_and_rotation_is_clockwise_sixteenths() {
        let mesh = map_decoration_mesh(Mat4::IDENTITY, 15, &[placement(20, -10, 4)]).unwrap();
        // Centre (74, 59); corner index 1 is the sprite's lower-right.
        let (p, _) = corner(&mesh, 1);
        let expected_x = (74.0 - 4.5) / 128.0 - 0.5;
        let expected_y = 0.5 - (59.0 + 3.5) / 128.0;
        assert!((p[0] - expected_x).abs() < 1.0e-6 && (p[1] - expected_y).abs() < 1.0e-6, "{p:?}");
    }

    /// Later icons sit further out, so overlapping icons keep their order.
    #[test]
    fn later_decorations_lie_in_front_of_earlier_ones() {
        let mesh = map_decoration_mesh(Mat4::IDENTITY, 15, &[placement(0, 0, 0), placement(0, 0, 0)]).unwrap();
        assert!(mesh.vertices[4].position[2] > mesh.vertices[0].position[2]);
        assert!(map_decoration_mesh(Mat4::IDENTITY, 15, &[]).is_none());
    }

    /// Only the five markers a player or map-maker places are hidden on an item
    /// frame, per the reference's registrations.
    #[test]
    fn only_player_style_markers_are_hidden_on_item_frames() {
        let hidden: Vec<&str> = MAP_DECORATION_TYPES
            .iter()
            .filter(|ty| !ty.show_on_item_frame)
            .map(|ty| ty.key)
            .collect();
        assert_eq!(
            hidden,
            ["player", "red_marker", "blue_marker", "player_off_map", "player_off_limits"]
        );
        assert_eq!(map_decoration_type("mansion").unwrap().sprite, "woodland_mansion");
        assert!(map_decoration_type("not_a_marker").is_none());
    }
}
