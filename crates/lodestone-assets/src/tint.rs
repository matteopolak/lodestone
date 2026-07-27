//! Biome tint resolution: colormaps, colour constants, and the seams that let a
//! version crate and the world drive per-vertex tinting without this crate
//! depending on either.
//!
//! # Why tinting lives partly behind traits
//!
//! Many blocks — grass, leaves, water, redstone, lily pads — multiply their
//! texture by a colour at mesh time. Vanilla resolves that colour from a block's
//! `tintindex` (carried on every [`BakedQuad`](crate::BakedQuad)) through two
//! pieces of knowledge:
//!
//! 1. **Which resolver a block uses** (grass colormap vs. foliage colormap vs.
//!    a constant vs. the biome water colour vs. the redstone power ramp). This
//!    is registered in the *client* jar package (`BlockColors`), which is **not**
//!    part of Mojang's deobfuscated *server* source, so it cannot be transcribed
//!    from the jar. It is therefore a version-crate responsibility, expressed
//!    here as the [`vanilla_tint_kind`] default plus a caller-supplied override.
//! 2. **Biome climate at a position** (temperature, downfall, water colour,
//!    grass modifier). That is a world/render concern, expressed by the
//!    [`BiomeTint`] seam.
//!
//! What *is* verifiable from the server source — and is implemented here exactly
//! — is the colormap sampling math ([`Colormap::sample`]), the foliage/grass
//! constants ([`colors`]), the [`GrassColorModifier`] formulae, and the
//! [`redstone_power_color`] ramp.
//!
//! The mechanism is deliberately GPU-free: [`Colormaps::resolve`] returns a plain
//! `0xRRGGBB` the mesher folds into vertex colours. Vanilla additionally blends
//! these point colours across a small biome radius; that averaging needs the
//! biome grid and so stays in the world/mesher, calling [`Colormaps::resolve`]
//! per sampled position.

use std::collections::BTreeMap;

use lodestone_model::{BlockPos, Identifier};

use crate::error::TintError;
use crate::manager::ResourceManager;
use crate::texture::Image;

/// A colour packed as `0x00RRGGBB` (no alpha; tints are opaque multipliers).
pub type Rgb = u32;

/// Colour constants verified against Mojang's server source
/// (`FoliageColor`, `DryFoliageColor`).
pub mod colors {
    use super::Rgb;

    /// `FoliageColor.FOLIAGE_DEFAULT` — colormap fallback and the default map
    /// colour for out-of-range samples.
    pub const FOLIAGE_DEFAULT: Rgb = 0x48B518;
    /// `FoliageColor.FOLIAGE_EVERGREEN` — spruce leaves (no biome tint).
    pub const FOLIAGE_EVERGREEN: Rgb = 0x619961;
    /// `FoliageColor.FOLIAGE_BIRCH` — birch leaves (no biome tint).
    pub const FOLIAGE_BIRCH: Rgb = 0x80A755;
    /// `FoliageColor.FOLIAGE_MANGROVE`. Note: mangrove *leaves* actually use the
    /// foliage colormap (per `BlockColors`); this constant is kept for
    /// completeness / other mangrove parts.
    pub const FOLIAGE_MANGROVE: Rgb = 0x92C648;
    /// `DryFoliageColor.FOLIAGE_DRY_DEFAULT` — dry foliage colormap fallback.
    pub const DRY_FOLIAGE_DEFAULT: Rgb = 0x5C3C32;
    /// `BlockColors.LILY_PAD_IN_WORLD` — lily pad's constant in-world tint.
    pub const LILY_PAD_IN_WORLD: Rgb = 0x208030;
}

/// A 256x256 biome colormap (`grass`, `foliage`, or `dry_foliage`), indexed by
/// temperature and downfall exactly as vanilla's `ColorMapColorUtil`.
#[derive(Clone, PartialEq, Eq)]
pub struct Colormap {
    /// Row-major `0xRRGGBB` pixels; typically 65 536 entries for a 256x256 map.
    pixels: Vec<Rgb>,
    /// Width in pixels, used for bounds when a map is not the usual 256 wide.
    width: usize,
    /// Height in pixels.
    height: usize,
    /// Fallback colour for an index outside the map (vanilla's default map
    /// colour argument).
    default: Rgb,
}

impl std::fmt::Debug for Colormap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Colormap")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("default", &format_args!("{:#08X}", self.default))
            .finish()
    }
}

impl Colormap {
    /// Builds a colormap from a decoded image, dropping alpha. `default` is the
    /// colour returned for a sample index outside the image (matching vanilla's
    /// per-colormap default map colour).
    ///
    /// # Errors
    ///
    /// Returns [`TintError::EmptyColormap`] if the image has zero area.
    pub fn from_image(image: &Image, default: Rgb) -> Result<Self, TintError> {
        if image.width == 0 || image.height == 0 {
            return Err(TintError::EmptyColormap);
        }
        let pixels = image
            .rgba
            .chunks_exact(4)
            .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
            .collect();
        Ok(Self {
            pixels,
            width: image.width as usize,
            height: image.height as usize,
            default,
        })
    }

    /// Samples the map for a temperature and downfall, following vanilla's
    /// `ColorMapColorUtil.get`: `rain *= temp; x = (1-temp)*255; y = (1-rain)*255;
    /// index = y<<8 | x`. Inputs are clamped to `[0, 1]` first (as `Biome` does).
    #[must_use]
    pub fn sample(&self, temperature: f32, downfall: f32) -> Rgb {
        let temp = f64::from(temperature.clamp(0.0, 1.0));
        let rain = f64::from(downfall.clamp(0.0, 1.0)) * temp;
        let x = ((1.0 - temp) * 255.0) as i32;
        let y = ((1.0 - rain) * 255.0) as i32;
        let index = ((y << 8) | x) as usize;
        // A full 256x256 map addresses every index; a smaller map (or a
        // narrower/taller one) falls back to the default, as vanilla does when
        // the flattened index runs past the pixel array.
        if index < self.pixels.len() && index < self.width * self.height {
            self.pixels[index]
        } else {
            self.default
        }
    }
}

/// The three biome colormaps loaded from a pack stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Colormaps {
    /// `textures/colormap/grass.png`.
    pub grass: Colormap,
    /// `textures/colormap/foliage.png`.
    pub foliage: Colormap,
    /// `textures/colormap/dry_foliage.png`.
    pub dry_foliage: Colormap,
}

impl Colormaps {
    /// Loads the grass, foliage and dry-foliage colormaps from the pack stack.
    ///
    /// # Errors
    ///
    /// Returns [`TintError`] if a colormap is missing or fails to decode.
    pub fn load(manager: &ResourceManager) -> Result<Self, TintError> {
        let grass = load_colormap(manager, "grass", colors::FOLIAGE_DEFAULT)?;
        let foliage = load_colormap(manager, "foliage", colors::FOLIAGE_DEFAULT)?;
        let dry_foliage = load_colormap(manager, "dry_foliage", colors::DRY_FOLIAGE_DEFAULT)?;
        Ok(Self {
            grass,
            foliage,
            dry_foliage,
        })
    }

    /// Resolves a [`TintKind`] to an `0xRRGGBB` colour at a position, using the
    /// [`BiomeTint`] seam for climate and per-biome overrides. Returns `None`
    /// for [`TintKind::None`] (an untinted quad).
    ///
    /// This is the *point* colour for one position. Vanilla blends these across
    /// a small biome radius; that averaging stays in the caller because it needs
    /// the biome grid.
    #[must_use]
    pub fn resolve(&self, kind: TintKind, biome: &dyn BiomeTint, pos: BlockPos) -> Option<Rgb> {
        match kind {
            TintKind::None => None,
            TintKind::Constant(rgb) => Some(rgb),
            TintKind::RedstonePower(power) => Some(redstone_power_color(power)),
            TintKind::Water => Some(biome.water_color(pos)),
            TintKind::Grass => {
                let base = biome.grass_override(pos).unwrap_or_else(|| {
                    self.grass
                        .sample(biome.temperature(pos), biome.downfall(pos))
                });
                let modifier = biome.grass_modifier(pos);
                Some(modifier.modify(base, biome.grass_modifier_noise(pos)))
            }
            TintKind::Foliage => Some(biome.foliage_override(pos).unwrap_or_else(|| {
                self.foliage
                    .sample(biome.temperature(pos), biome.downfall(pos))
            })),
            TintKind::DryFoliage => Some(biome.dry_foliage_override(pos).unwrap_or_else(|| {
                self.dry_foliage
                    .sample(biome.temperature(pos), biome.downfall(pos))
            })),
        }
    }
}

fn load_colormap(
    manager: &ResourceManager,
    name: &str,
    default: Rgb,
) -> Result<Colormap, TintError> {
    let loc = crate::location::ResourceLocation::new("minecraft", format!("colormap/{name}"))
        .map_err(|_| TintError::MissingColormap {
            name: name.to_string(),
        })?;
    let bytes =
        manager
            .read_asset(&loc, "textures", "png")
            .ok_or_else(|| TintError::MissingColormap {
                name: name.to_string(),
            })?;
    let image = Image::decode_png(&bytes)?;
    Colormap::from_image(&image, default)
}

/// How a block's `tintindex` resolves to a colour.
///
/// The mapping from a block to its kind is client render behaviour (see the
/// module docs); [`vanilla_tint_kind`] provides a best-known default a version
/// crate may override. `RedstonePower` and any age-derived constants are folded
/// in by the classifier from block state, so [`Colormaps::resolve`] never needs
/// block properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TintKind {
    /// No tint — render the texture as-is.
    None,
    /// Biome grass colormap (with grass modifier and override applied).
    Grass,
    /// Biome foliage colormap.
    Foliage,
    /// Biome dry-foliage colormap.
    DryFoliage,
    /// The biome's flat water colour.
    Water,
    /// A fixed colour (evergreen/birch/mangrove leaves, lily pad, or a
    /// state-derived constant a version crate computed).
    Constant(Rgb),
    /// The redstone power ramp for a wire at the given power `0..=15`.
    RedstonePower(u8),
}

/// The biome inputs tint resolution needs at a position. Implemented by the
/// world/render layer; this crate never touches `lodestone-world`.
///
/// Only [`temperature`](BiomeTint::temperature),
/// [`downfall`](BiomeTint::downfall) and [`water_color`](BiomeTint::water_color)
/// are required; the override and modifier hooks default to "no override" /
/// "no modifier" so a minimal world can implement three methods.
pub trait BiomeTint: std::fmt::Debug {
    /// Biome temperature at `pos` (typically `0..=1` after height adjustment).
    fn temperature(&self, pos: BlockPos) -> f32;
    /// Biome downfall at `pos`.
    fn downfall(&self, pos: BlockPos) -> f32;
    /// The biome's flat water colour as `0xRRGGBB`.
    fn water_color(&self, pos: BlockPos) -> Rgb;
    /// A biome `grass_color` override, if the biome defines one.
    fn grass_override(&self, _pos: BlockPos) -> Option<Rgb> {
        None
    }
    /// A biome `foliage_color` override, if the biome defines one.
    fn foliage_override(&self, _pos: BlockPos) -> Option<Rgb> {
        None
    }
    /// A biome `dry_foliage_color` override, if the biome defines one.
    fn dry_foliage_override(&self, _pos: BlockPos) -> Option<Rgb> {
        None
    }
    /// The biome's grass colour modifier (swamp/dark-forest).
    fn grass_modifier(&self, _pos: BlockPos) -> GrassColorModifier {
        GrassColorModifier::None
    }
    /// The `Biome.BIOME_INFO_NOISE` sample the swamp modifier needs at `pos`
    /// (`noise(x*0.0225, z*0.0225)`). Only consulted for swamp biomes.
    fn grass_modifier_noise(&self, _pos: BlockPos) -> f64 {
        0.0
    }
}

/// A biome's grass colour modifier, matching
/// `BiomeSpecialEffects.GrassColorModifier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrassColorModifier {
    /// No modification.
    #[default]
    None,
    /// Dark forest: average the base colour toward `0x28340A`.
    DarkForest,
    /// Swamp: a noise-driven two-tone that ignores the base colour.
    Swamp,
}

impl GrassColorModifier {
    /// Applies the modifier to a base grass colour. `noise` is the
    /// `Biome.BIOME_INFO_NOISE` value the swamp variant needs (ignored by the
    /// others).
    #[must_use]
    pub fn modify(self, base: Rgb, noise: f64) -> Rgb {
        match self {
            GrassColorModifier::None => base & 0xFFFFFF,
            // ARGB.opaque((base & 0xFEFEFE) + 0x28340A >> 1).
            GrassColorModifier::DarkForest => {
                (((base & 0x00FE_FEFE) + 0x0028_340A) >> 1) & 0xFFFFFF
            }
            // groundValue < -0.1 -> 0x4C763C else 0x6A7039.
            GrassColorModifier::Swamp => {
                if noise < -0.1 {
                    0x4C763C
                } else {
                    0x6A7039
                }
            }
        }
    }
}

/// The redstone wire power ramp, `power` in `0..=15`, verified against
/// `RedStoneWireBlock.COLORS`.
#[must_use]
pub fn redstone_power_color(power: u8) -> Rgb {
    let p = f32::from(power.min(15)) / 15.0;
    let red = p * 0.6 + if p > 0.0 { 0.4 } else { 0.3 };
    let green = (p * p * 0.7 - 0.5).clamp(0.0, 1.0);
    let blue = (p * p * 0.6 - 0.7).clamp(0.0, 1.0);
    let to8 = |f: f32| (f * 255.0 + 0.5) as u32 & 0xFF;
    (to8(red) << 16) | (to8(green) << 8) | to8(blue)
}

/// Classifies a block, its `tintindex` layer and its state into a [`TintKind`],
/// matching vanilla's `BlockColors.createDefault()` registrations.
///
/// Verified against the client source (`net.minecraft.client.color.block.
/// BlockColors`/`BlockTintSources`). The `tintindex` is the *layer*: most blocks
/// have one layer at index 0, but `pink_petals`/`wildflowers` register
/// `[blank, grass]`, so their index 1 is grass-tinted while index 0 is untinted.
/// Blocks that carry a `tintindex` in their model but are *not* registered here
/// (e.g. `bamboo`, `cherry_leaves`, `pale_oak_leaves`, the non-water cauldrons)
/// are genuinely untinted and return [`TintKind::None`] — carrying an index is
/// not the same as being tinted.
#[must_use]
pub fn vanilla_tint_kind(
    block: &Identifier,
    tint_index: i32,
    properties: &BTreeMap<String, String>,
) -> TintKind {
    if tint_index < 0 {
        return TintKind::None;
    }
    let path = block.path();
    match (path, tint_index) {
        // Grass colormap (BiomeColors.getAverageGrassColor). `tall_grass`/
        // `large_fern` sample the lower half's position in the mesher, but the
        // colormap kind is the same.
        (
            "grass_block" | "short_grass" | "fern" | "potted_fern" | "bush" | "tall_grass"
            | "large_fern" | "sugar_cane",
            0,
        ) => TintKind::Grass,
        // pink_petals / wildflowers register [blank, grass]: index 0 untinted,
        // index 1 grass.
        ("pink_petals" | "wildflowers", 0) => TintKind::None,
        ("pink_petals" | "wildflowers", 1) => TintKind::Grass,
        // Foliage colormap (getAverageFoliageColor) — mangrove and vine included.
        (
            "oak_leaves" | "jungle_leaves" | "acacia_leaves" | "dark_oak_leaves" | "vine"
            | "mangrove_leaves",
            0,
        ) => TintKind::Foliage,
        // Dry-foliage colormap (getAverageDryFoliageColor).
        ("leaf_litter", 0) => TintKind::DryFoliage,
        // Constant-colour leaves (no biome tint).
        ("spruce_leaves", 0) => TintKind::Constant(colors::FOLIAGE_EVERGREEN),
        ("birch_leaves", 0) => TintKind::Constant(colors::FOLIAGE_BIRCH),
        // Water: biome water colour (getAverageWaterColor). The fluid *surface*
        // is tinted the same way via the fluid model's tint source.
        ("water_cauldron", 0) => TintKind::Water,
        // Redstone wire power ramp.
        ("redstone_wire", _) => {
            let power = properties
                .get("power")
                .and_then(|p| p.parse::<u8>().ok())
                .unwrap_or(0);
            TintKind::RedstonePower(power)
        }
        // Growing stems fade green→brown with age; attached stems are constant.
        ("melon_stem" | "pumpkin_stem", 0) => {
            let age = properties
                .get("age")
                .and_then(|p| p.parse::<u8>().ok())
                .unwrap_or(0);
            TintKind::Constant(stem_color(age))
        }
        ("attached_melon_stem" | "attached_pumpkin_stem", 0) => TintKind::Constant(stem_color(7)),
        // Lily pad: constant in-world colour.
        ("lily_pad", 0) => TintKind::Constant(colors::LILY_PAD_IN_WORLD),
        _ => TintKind::None,
    }
}

/// The stem tint for `age` (0..=7), matching `BlockTintSources.stem`:
/// `ARGB.color(age*32, 255 - age*8, age*4)`.
#[must_use]
pub fn stem_color(age: u8) -> Rgb {
    let a = u32::from(age);
    let r = (a * 32) & 0xFF;
    let g = (255 - a * 8) & 0xFF;
    let b = (a * 4) & 0xFF;
    (r << 16) | (g << 8) | b
}
