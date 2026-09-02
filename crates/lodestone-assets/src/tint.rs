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

    /// Vanilla's own foliage-color "foliage default" constant — colormap fallback and the default map
    /// colour for out-of-range samples.
    pub const FOLIAGE_DEFAULT: Rgb = 0x48B518;
    /// Vanilla's own foliage-color "foliage evergreen" constant — spruce leaves (no biome tint).
    pub const FOLIAGE_EVERGREEN: Rgb = 0x619961;
    /// Vanilla's own foliage-color "foliage birch" constant — birch leaves (no biome tint).
    pub const FOLIAGE_BIRCH: Rgb = 0x80A755;
    /// Vanilla's own foliage-color "foliage mangrove" constant. Note: mangrove *leaves* actually use the
    /// foliage colormap (per vanilla's own block-colors registration); this constant is kept for
    /// completeness / other mangrove parts.
    pub const FOLIAGE_MANGROVE: Rgb = 0x92C648;
    /// Vanilla's own dry-foliage-color "foliage dry default" constant — dry foliage colormap fallback.
    pub const DRY_FOLIAGE_DEFAULT: Rgb = 0x5C3C32;
    /// Vanilla's own block-colors "lily pad in world" constant — lily pad's constant in-world tint.
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

    /// Samples the map for a temperature and downfall, following vanilla's own
    /// color-map-color-util "get" step: `rain *= temp; x = (1-temp)*255; y = (1-rain)*255;
    /// index = y<<8 | x`. Inputs are clamped to `[0, 1]` first (as vanilla's own biome class does).
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
    /// Vanilla's own biome-info-noise sample the swamp modifier needs at `pos`
    /// (`noise(x*0.0225, z*0.0225)`). Only consulted for swamp biomes.
    fn grass_modifier_noise(&self, _pos: BlockPos) -> f64 {
        0.0
    }
}

/// A biome's grass colour modifier, matching vanilla's own
/// biome-special-effects grass-color-modifier enum.
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
    /// vanilla biome-info-noise value the swamp variant needs (ignored by the
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
/// vanilla's own redstone-wire-block colors table.
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
/// matching vanilla's own block-colors "create default" registrations.
///
/// Verified against the client source (vanilla's own block-color and
/// block-tint-source registration classes). The `tintindex` is the *layer*: most blocks
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

/// The tint a **break/hit particle** of `block` takes, matching vanilla's own
/// "color as terrain particle" accessor at layer 0 — what
/// `TerrainParticle`'s constructor multiplies its `0.6` grey by.
///
/// This is *not* the same lookup as `vanilla_tint_kind(block, 0, …)`. In-world
/// face tinting and particle tinting are separate virtual methods on
/// vanilla's own block-tint-source base class, and two registrations override
/// the particle one to
/// disagree with the in-world one (vanilla's own block-tint-sources
/// registration class, 26.2):
///
/// * **`grass_block`** — vanilla's own grass-block tint source overrides its
///   "color as terrain particle" accessor to
///   return `-1` (untinted). It has to: `grass_block`'s `#particle` variable is
///   `block/dirt`, so applying the grass colormap would throw *green dirt*.
///   This is the same special case the pre-26.x client spelled out inline as
///   `if (!state.is(Blocks.GRASS_BLOCK))`.
/// * **`water` / `bubble_column`** — vanilla's own water-particles tint source
///   is the mirror image:
///   `color`/its own "color in world" accessor are `-1` (the fluid *surface* is
///   tinted by the fluid
///   model instead, which is why [`vanilla_tint_kind`] reports `None` for them)
///   while its own "color as terrain particle" accessor returns the biome water colour.
///
/// Every other registration inherits its "color as terrain particle" accessor
/// from
/// its "color in world" accessor, so it agrees with [`vanilla_tint_kind`] at
/// layer 0 — hence
/// the delegation rather than a second copy of the whole table.
///
/// Getting this wrong is not subtle on screen but *is* subtle in review: the
/// tinted blocks are exactly the ones whose sprites are **greyscale** in the
/// atlas (`grass`, `fern`, the leaves, `sugar_cane`, `redstone_dust_*`), so a
/// missing tint renders their debris as near-white flecks rather than as an
/// obviously wrong colour.
#[must_use]
pub fn vanilla_particle_tint_kind(
    block: &Identifier,
    properties: &BTreeMap<String, String>,
) -> TintKind {
    match block.path() {
        "grass_block" => TintKind::None,
        "water" | "bubble_column" => TintKind::Water,
        _ => vanilla_tint_kind(block, 0, properties),
    }
}

/// The stem tint for `age` (0..=7), matching vanilla's own block-tint-sources
/// stem provider:
/// `ARGB.color(age*32, 255 - age*8, age*4)`.
#[must_use]
pub fn stem_color(age: u8) -> Rgb {
    let a = u32::from(age);
    let r = (a * 32) & 0xFF;
    let g = (255 - a * 8) & 0xFF;
    let b = (a * 4) & 0xFF;
    (r << 16) | (g << 8) | b
}

/// A biome's `effects` compound plus its top-level `temperature`/`downfall` —
/// everything [`BiomeTint`] needs for one biome, read directly off the biome's
/// own definition rather than sampled from a colormap PNG.
///
/// Verified against `.cache/mc/26.2/src/data/minecraft/worldgen/biome/*.json`
/// (Mojang's own generated data, tier 1 in `CLAUDE.md`'s data-source order) —
/// every one of the 66 files gated by `docs/worldgen-biomes.md`'s "66/66"
/// check. `water_color`, `grass_color`, `foliage_color` and
/// `dry_foliage_color` are `effects.*_color`; `grass_modifier` is
/// `effects.grass_color_modifier`; `temperature`/`downfall` are the
/// biome-level fields vanilla's own biome climate-settings record reads (see
/// [`Colormap::sample`]'s doc for why those two, not the override colours,
/// still need clamping downstream).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiomeEffects {
    /// Declared `temperature`, matching vanilla's own "get base temperature" accessor.
    pub temperature: f32,
    /// Declared `downfall`.
    pub downfall: f32,
    /// `effects.water_color` — every 26.2 biome declares one (no `Option`,
    /// unlike the three colormap overrides below).
    pub water_color: Rgb,
    /// `effects.grass_color`, when the biome overrides the grass colormap
    /// (badlands' dead grass, the cherry blossoms' pink, etc.).
    pub grass_color: Option<Rgb>,
    /// `effects.foliage_color` override.
    pub foliage_color: Option<Rgb>,
    /// `effects.dry_foliage_color` override.
    pub dry_foliage_color: Option<Rgb>,
    /// `effects.grass_color_modifier` (`"swamp"`/`"dark_forest"`/absent).
    pub grass_modifier: GrassColorModifier,
}

/// Every 26.2 biome's [`BiomeEffects`], keyed by its path (no `minecraft:`
/// namespace — see [`biome_effects`]).
///
/// **This table must stay in strictly ascending `str` order (bytewise, which is
/// what `Ord for str` is), because [`FIRST_BYTE_INDEX`] assumes entries sharing a
/// first byte are contiguous.** That was not a requirement before — the order used
/// to be incidental, a by-product of the alphabetical `worldgen/biome/*.json`
/// listing it was generated from, and the old doc said so explicitly. A biome
/// inserted in the wrong place now silently resolves to `None` (so it renders the
/// plains fallback) rather than failing to compile, which is why
/// `biome_effects_table_is_strictly_ascending` in this module's tests enforces it.
///
/// Strictly speaking only *grouping* by first byte is needed, not a full sort; the
/// full sort is asserted because it is the stronger, simpler property to state and
/// to restore. Note `_` is `0x5F`, *below* every lowercase letter, so
/// "alphabetical ignoring underscores" is **not** the required order — sort with
/// `LC_ALL=C` if you regenerate this by hand.
#[rustfmt::skip]
const BIOME_EFFECTS: &[(&str, BiomeEffects)] = &[
    ("badlands", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: Some(0x90814D), foliage_color: Some(0x9E814D), dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("bamboo_jungle", BiomeEffects { temperature: 0.95f32, downfall: 0.9f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("basalt_deltas", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("beach", BiomeEffects { temperature: 0.8f32, downfall: 0.4f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("birch_forest", BiomeEffects { temperature: 0.6f32, downfall: 0.6f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("cherry_grove", BiomeEffects { temperature: 0.5f32, downfall: 0.8f32, water_color: 0x5DB7EF, grass_color: Some(0xB6DB61), foliage_color: Some(0xB6DB61), dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("cold_ocean", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3D57D6, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("crimson_forest", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("dark_forest", BiomeEffects { temperature: 0.7f32, downfall: 0.8f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: Some(0x7B5334), grass_modifier: GrassColorModifier::DarkForest }),
    ("deep_cold_ocean", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3D57D6, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("deep_dark", BiomeEffects { temperature: 0.8f32, downfall: 0.4f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("deep_frozen_ocean", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3938C9, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("deep_lukewarm_ocean", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x45ADF2, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("deep_ocean", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("desert", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("dripstone_caves", BiomeEffects { temperature: 0.8f32, downfall: 0.4f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("end_barrens", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("end_highlands", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("end_midlands", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("eroded_badlands", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: Some(0x90814D), foliage_color: Some(0x9E814D), dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("flower_forest", BiomeEffects { temperature: 0.7f32, downfall: 0.8f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("forest", BiomeEffects { temperature: 0.7f32, downfall: 0.8f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("frozen_ocean", BiomeEffects { temperature: 0.0f32, downfall: 0.5f32, water_color: 0x3938C9, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("frozen_peaks", BiomeEffects { temperature: -0.7f32, downfall: 0.9f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("frozen_river", BiomeEffects { temperature: 0.0f32, downfall: 0.5f32, water_color: 0x3938C9, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("grove", BiomeEffects { temperature: -0.2f32, downfall: 0.8f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("ice_spikes", BiomeEffects { temperature: 0.0f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("jagged_peaks", BiomeEffects { temperature: -0.7f32, downfall: 0.9f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("jungle", BiomeEffects { temperature: 0.95f32, downfall: 0.9f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("lukewarm_ocean", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x45ADF2, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("lush_caves", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("mangrove_swamp", BiomeEffects { temperature: 0.8f32, downfall: 0.9f32, water_color: 0x3A7A6A, grass_color: None, foliage_color: Some(0x8DB127), dry_foliage_color: Some(0x7B5334), grass_modifier: GrassColorModifier::Swamp }),
    ("meadow", BiomeEffects { temperature: 0.5f32, downfall: 0.8f32, water_color: 0x0E4ECF, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("mushroom_fields", BiomeEffects { temperature: 0.9f32, downfall: 1.0f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("nether_wastes", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("ocean", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("old_growth_birch_forest", BiomeEffects { temperature: 0.6f32, downfall: 0.6f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("old_growth_pine_taiga", BiomeEffects { temperature: 0.3f32, downfall: 0.8f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("old_growth_spruce_taiga", BiomeEffects { temperature: 0.25f32, downfall: 0.8f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("pale_garden", BiomeEffects { temperature: 0.7f32, downfall: 0.8f32, water_color: 0x76889D, grass_color: Some(0x778272), foliage_color: Some(0x878D76), dry_foliage_color: Some(0xA0A69C), grass_modifier: GrassColorModifier::None }),
    ("plains", BiomeEffects { temperature: 0.8f32, downfall: 0.4f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("river", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("savanna", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("savanna_plateau", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("small_end_islands", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("snowy_beach", BiomeEffects { temperature: 0.05f32, downfall: 0.3f32, water_color: 0x3D57D6, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("snowy_plains", BiomeEffects { temperature: 0.0f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("snowy_slopes", BiomeEffects { temperature: -0.3f32, downfall: 0.9f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("snowy_taiga", BiomeEffects { temperature: -0.5f32, downfall: 0.4f32, water_color: 0x3D57D6, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("soul_sand_valley", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("sparse_jungle", BiomeEffects { temperature: 0.95f32, downfall: 0.8f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("stony_peaks", BiomeEffects { temperature: 1.0f32, downfall: 0.3f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("stony_shore", BiomeEffects { temperature: 0.2f32, downfall: 0.3f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("sulfur_caves", BiomeEffects { temperature: 0.8f32, downfall: 0.4f32, water_color: 0x34BF89, grass_color: Some(0xABA64F), foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("sunflower_plains", BiomeEffects { temperature: 0.8f32, downfall: 0.4f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("swamp", BiomeEffects { temperature: 0.8f32, downfall: 0.9f32, water_color: 0x617B64, grass_color: None, foliage_color: Some(0x6A7039), dry_foliage_color: Some(0x7B5334), grass_modifier: GrassColorModifier::Swamp }),
    ("taiga", BiomeEffects { temperature: 0.25f32, downfall: 0.8f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("the_end", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("the_void", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("warm_ocean", BiomeEffects { temperature: 0.5f32, downfall: 0.5f32, water_color: 0x43D5EE, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("warped_forest", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("windswept_forest", BiomeEffects { temperature: 0.2f32, downfall: 0.3f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("windswept_gravelly_hills", BiomeEffects { temperature: 0.2f32, downfall: 0.3f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("windswept_hills", BiomeEffects { temperature: 0.2f32, downfall: 0.3f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("windswept_savanna", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: None, foliage_color: None, dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
    ("wooded_badlands", BiomeEffects { temperature: 2.0f32, downfall: 0.0f32, water_color: 0x3F76E4, grass_color: Some(0x90814D), foliage_color: Some(0x9E814D), dry_foliage_color: None, grass_modifier: GrassColorModifier::None }),
];

/// For each possible first byte, the half-open `BIOME_EFFECTS` range of entries
/// beginning with it — the index [`biome_effects`] uses instead of scanning all
/// 66.
///
/// Computed from the table at compile time by [`build_first_byte_index`], so it
/// cannot drift out of step with it. Correct **only** because [`BIOME_EFFECTS`] is
/// sorted, which makes entries sharing a first byte contiguous; see that table's
/// doc for the invariant and the test enforcing it.
///
/// `u8` bounds are enough because the table has 66 entries. A first byte with no
/// biome gets an empty range, which the scan below handles with no special case.
const FIRST_BYTE_INDEX: [(u8, u8); 256] = build_first_byte_index();

/// Buckets [`BIOME_EFFECTS`] by first byte at compile time. A `const fn` rather
/// than a hand-written table precisely so adding a biome cannot leave a stale
/// index behind — the failure mode would be one biome silently rendering the
/// plains fallback, which no compile error and no screenshot would catch.
const fn build_first_byte_index() -> [(u8, u8); 256] {
    let mut index = [(0u8, 0u8); 256];
    let mut i = 0usize;
    while i < BIOME_EFFECTS.len() {
        let byte = BIOME_EFFECTS[i].0.as_bytes()[0];
        let start = i;
        while i < BIOME_EFFECTS.len() && BIOME_EFFECTS[i].0.as_bytes()[0] == byte {
            i += 1;
        }
        index[byte as usize] = (start as u8, i as u8);
    }
    index
}

/// Looks up a biome's [`BiomeEffects`] by name. Accepts either the bare path
/// (`"swamp"`) or the namespaced id (`"minecraft:swamp"`) — a caller reading a
/// wire biome name doesn't have to strip the namespace first.
///
/// # Why the shape of this function matters, and why it is not a binary search
///
/// This is not a cold path. `DESIGN.md` §12.124 measured one
/// `NamedBiomeTint::water_color` call at **6,263 instructions, 97.8% of it inside
/// this function**, because vanilla's biome blend is a radius-2 box — 25 samples
/// per tinted quad — and `mesh_models`'s grass path asks up to four questions
/// *per sample*. It was a plain `BIOME_EFFECTS.iter().find(…)`, ~33 compares on
/// an average hit and all 66 on a miss.
///
/// **A `binary_search_by` over the sorted table was tried and measured *worse*,
/// and the reason generalises: not all string compares cost the same.** `find`'s
/// `*name == path` is `str::eq`, which compares **lengths first** and only reaches
/// `memcmp` when they match — measured at **8.6 instructions per entry** on this
/// table, because most entries differ from the probe in length. A `binary_search`
/// comparator must return an `Ordering`, so every one of its ~7 probes is a real
/// lexicographic `memcmp` call. Measured per call (§12.126): 58 → **309**
/// instructions for the table's first entry, 618 → 352 for its last, and
/// `mesh_fluids` **regressed 6,629 → 6,815 instructions per fluid cell**. Seven
/// expensive compares beat thirty-three cheap ones only if you never price them.
///
/// So the win comes from doing *fewer of the cheap compares*: a compile-time
/// [`FIRST_BYTE_INDEX`] narrows the scan to the entries sharing the probe's first
/// byte — 3.79 compares on an average hit against 33.5, and a *miss* usually
/// resolves in one or none. The miss path is worth naming separately because
/// `NamedBiomeTint` deliberately does not memoise unresolvable names, so a section
/// whose biome id is past `FALLBACK_BIOME_NAMES` paid a full 66-entry scan on
/// every one of its 25 samples per cell.
#[must_use]
pub fn biome_effects(id: &str) -> Option<&'static BiomeEffects> {
    let path = id.strip_prefix("minecraft:").unwrap_or(id);
    let (start, end) = FIRST_BYTE_INDEX[*path.as_bytes().first()? as usize];
    BIOME_EFFECTS[start as usize..end as usize]
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, effects)| effects)
}

/// Vanilla's biome-tint smoothing kernel: a `(2*radius+1)²` box average of a
/// per-position colour, sampled at `radius`-many neighbours on each side of
/// `(x, z)` at a fixed height. Matches vanilla's own client-level "calculate
/// block tint" step
/// exactly, including its per-channel integer
/// (floor) division — `sample` should already be vanilla's un-blended
/// color-resolver "get color" accessor for one position (i.e. one
/// [`Colormaps::resolve`]
/// call), and this function performs the averaging **around** it, matching
/// the split between vanilla's own biome "get grass color"/"get foliage
/// color"/"get water color" accessors
/// (one point) and its own "calculate block tint" step (the box that wraps them).
///
/// `radius: 0` skips the loop entirely and returns `sample(x, z)` directly —
/// vanilla's own `dist == 0` fast path, which the "Fast" video-settings
/// preset selects, and which incidentally makes `radius: 0` and *no* biome
/// tint (a single-sample world) diverge only in what `sample` does, not in
/// this function's control flow.
///
/// The default biome-blend-radius option value is `2` (vanilla's own options class's own
/// biome-blend-radius option's
/// `new OptionInstance.IntRange(0, 7, false), 2, …`), giving the vanilla
/// default 5x5 = 25-sample average this crate's callers should use unless a
/// video setting says otherwise (this client has no such setting yet, so `2`
/// is not a guess — it is the only value reachable).
#[must_use]
pub fn blend_box<F: FnMut(i32, i32) -> Rgb>(x: i32, z: i32, radius: i32, mut sample: F) -> Rgb {
    if radius <= 0 {
        return sample(x, z);
    }
    let count = ((radius * 2 + 1) * (radius * 2 + 1)) as u32;
    let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            let c = sample(x + dx, z + dz);
            r += (c >> 16) & 0xFF;
            g += (c >> 8) & 0xFF;
            b += c & 0xFF;
        }
    }
    ((r / count) << 16) | ((g / count) << 8) | (b / count)
}

/// Vanilla's default biome-blend radius (vanilla's own options class's own
/// biome-blend-radius option). See
/// [`blend_box`]'s doc for why this is the only reachable value right now.
pub const DEFAULT_BLEND_RADIUS: i32 = 2;

/// The largest value vanilla's own biome-blend-radius option exposes — its
/// `new OptionInstance.IntRange(0, 7, false)`. It bounds [`BlendRowCursor`]'s
/// window, which is why that type needs no allocation.
pub const MAX_BLEND_RADIUS: i32 = 7;

/// Window width for [`MAX_BLEND_RADIUS`], the size of [`BlendRowCursor`]'s ring.
const MAX_BLEND_WIDTH: usize = (2 * MAX_BLEND_RADIUS + 1) as usize;

/// [`blend_box`], evaluated incrementally along a row of constant `z`.
///
/// # What it is for
///
/// Two horizontally adjacent cells' radius-2 boxes overlap in **20 of their 25
/// columns**, and vanilla's per-channel floor division happens once at the end
/// rather than per sample — so a running per-channel sum over a sliding window of
/// *column* sums yields the same `Rgb` for **5** new `sample` calls per step
/// instead of 25. `mesh_fluids` and `mesh_models` both iterate `y → z → x` with
/// `x` innermost (`models.rs`), which is exactly the order this exploits. The
/// biome tint was still ~63% of `mesh_fluids`'s per-cell cost after an earlier
/// optimisation pass (`DESIGN.md` §12.124), and this is where that goes.
///
/// # Why it is bit-identical, not merely close
///
/// [`blend_box`] computes each channel as `Σ_dz Σ_dx byte`; this computes
/// `Σ_dx (Σ_dz byte)`. Both are exact `u32` sums of the *same* `(2r+1)²` bytes:
/// integer addition is associative, and the largest possible total is
/// `15² × 255 = 57,375`, five orders of magnitude below `u32::MAX`, so no
/// intermediate rounds or wraps. The subtraction on a slide removes a column that
/// the ring invariant guarantees is currently *part* of the total, so it cannot
/// underflow. The final `/ count` is the same expression on the same integer.
/// Nothing here is floating point and nothing divides early — the two properties
/// that would make a reassociated blend drift by a byte, invisibly.
///
/// **Consequently `sample` must be pure.** A counting or logging closure will see
/// a different number of calls in a different order, which is the point of the
/// type; a closure whose *return value* depends on how often it was called will
/// produce a different colour. `crate::tint`'s callers pass a pure
/// `Colormaps::resolve`.
///
/// # How to change it
///
/// The ring invariant is: `cols[(head + i) % width]` holds the column sums for
/// `x - radius + i`, for `i in 0..width`. Every method preserves it, and
/// `blend_row_cursor_is_bit_identical_to_blend_box` in `tests/tint.rs` walks rows
/// forward, backward and in jumps at five radii against [`blend_box`] itself —
/// keep that reference arm, because it is the implementation this replaces and
/// the only outside expectation available for it.
///
/// A `z` different from the loaded window's, or a jump of `width` or more in `x`,
/// rebuilds from scratch (`width²` samples, i.e. exactly [`blend_box`]) rather
/// than sliding, so the cursor is never *worse* than the function it wraps. State
/// that does not belong to the current caller must be dropped with
/// [`invalidate`](Self::invalidate) — the cursor keys itself on `(x, z)` only, so
/// anything else the sample closure depends on (which tint kind, which `y`) is the
/// caller's to track. `lodestone_render::biome_tint::BlendedTintCursor` is the
/// worked example.
#[derive(Debug, Clone)]
pub struct BlendRowCursor {
    radius: i32,
    /// `2 * radius + 1`, cached because it indexes the ring on every call.
    width: usize,
    /// Per-channel sums for one column each (`2 * radius + 1` samples at fixed
    /// `x`), in ring order — see the invariant in the type docs.
    cols: [[u32; 3]; MAX_BLEND_WIDTH],
    head: usize,
    /// `Σ cols[..width]`, maintained incrementally.
    total: [u32; 3],
    /// The centre the loaded window belongs to, or `None` when empty.
    at: Option<(i32, i32)>,
}

impl BlendRowCursor {
    /// A cursor for a fixed `radius`, clamped to `0..=MAX_BLEND_RADIUS`. The
    /// radius is fixed at construction rather than passed per call so a caller
    /// cannot silently mix two radii into one window.
    #[must_use]
    pub fn new(radius: i32) -> Self {
        let radius = radius.clamp(0, MAX_BLEND_RADIUS);
        Self {
            radius,
            width: (radius * 2 + 1) as usize,
            cols: [[0; 3]; MAX_BLEND_WIDTH],
            head: 0,
            total: [0; 3],
            at: None,
        }
    }

    /// The radius this cursor was built with, after clamping.
    #[must_use]
    pub const fn radius(&self) -> i32 {
        self.radius
    }

    /// Drops the loaded window, so the next [`blend`](Self::blend) rebuilds. Call
    /// this whenever anything the `sample` closure depends on changes other than
    /// the `(x, z)` this type tracks itself.
    pub fn invalidate(&mut self) {
        self.at = None;
    }

    /// The blended colour at `(x, z)` — bit-identical to
    /// `blend_box(x, z, self.radius(), sample)`, at 5 samples per single-step
    /// move along the row instead of 25.
    pub fn blend<F: FnMut(i32, i32) -> Rgb>(&mut self, x: i32, z: i32, mut sample: F) -> Rgb {
        if self.radius <= 0 {
            // `blend_box`'s own `radius <= 0` fast path, and the same result.
            return sample(x, z);
        }
        let width = self.width;
        // Sliding costs `|dx| * width` samples against a rebuild's `width²`, so it
        // is only taken while `|dx| < width` — which is what makes the cursor
        // never more expensive than the function it wraps.
        let slide_from = self.at.filter(|&(cx, cz)| {
            cz == z && ((x - cx).unsigned_abs() as usize) < width
        });
        if let Some((mut cx, _)) = slide_from {
            while cx < x {
                // The column at `cx - radius` leaves and the one at
                // `cx + 1 + radius` enters, and both are ring slot `head`.
                let col = self.column(cx + 1 + self.radius, z, &mut sample);
                self.replace(self.head, col);
                self.head = (self.head + 1) % width;
                cx += 1;
            }
            while cx > x {
                let slot = (self.head + width - 1) % width;
                let col = self.column(cx - 1 - self.radius, z, &mut sample);
                self.replace(slot, col);
                self.head = slot;
                cx -= 1;
            }
        } else {
            self.total = [0; 3];
            self.head = 0;
            for i in 0..width {
                let col = self.column(x - self.radius + i as i32, z, &mut sample);
                self.cols[i] = col;
                for c in 0..3 {
                    self.total[c] += col[c];
                }
            }
        }
        self.at = Some((x, z));
        // The same division `blend_box` performs, on the same integer.
        let count = (width * width) as u32;
        ((self.total[0] / count) << 16) | ((self.total[1] / count) << 8) | (self.total[2] / count)
    }

    /// Swaps one ring slot, keeping [`total`](Self::total) exact. The subtraction
    /// cannot underflow: `cols[slot]` is part of `total` by the ring invariant.
    fn replace(&mut self, slot: usize, col: [u32; 3]) {
        for c in 0..3 {
            self.total[c] = self.total[c] - self.cols[slot][c] + col[c];
        }
        self.cols[slot] = col;
    }

    /// One column's per-channel sums: `2 * radius + 1` samples at fixed `cx`.
    fn column<F: FnMut(i32, i32) -> Rgb>(&self, cx: i32, z: i32, sample: &mut F) -> [u32; 3] {
        let mut col = [0u32; 3];
        for dz in -self.radius..=self.radius {
            let c = sample(cx, z + dz);
            col[0] += (c >> 16) & 0xFF;
            col[1] += (c >> 8) & 0xFF;
            col[2] += c & 0xFF;
        }
        col
    }
}

/// Tests for invariants of *private* items — [`BIOME_EFFECTS`]'s sort order in
/// particular, which the crate's public surface cannot see. Everything testable
/// through the public API lives in `tests/tint.rs`.
#[cfg(test)]
mod tests {
    /// The first index at which `names` is not strictly ascending under
    /// `Ord for str`, or `None`. Separate from the test so the control below can
    /// point it at data known to be out of order — an ascending-check that always
    /// returns `None` would make the real test vacuous and read identically.
    fn first_disorder(names: &[&str]) -> Option<usize> {
        (1..names.len()).find(|&i| names[i - 1] >= names[i])
    }

    /// [`super::FIRST_BYTE_INDEX`] buckets [`super::BIOME_EFFECTS`] on the
    /// assumption that entries sharing a first byte are contiguous, so a biome
    /// inserted in the wrong place silently stops resolving. Nothing else enforces
    /// the order: it is a `const` array of literals and the compiler has no
    /// opinion about it.
    #[test]
    fn biome_effects_table_is_strictly_ascending() {
        let names: Vec<&str> = super::BIOME_EFFECTS.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names.len(),
            66,
            "the 26.2 biome set is 66 entries (docs/worldgen-biomes.md's 66/66 gate); a \
             different length means this test and the table disagree about the subject"
        );
        assert_eq!(
            first_disorder(&names),
            None,
            "BIOME_EFFECTS is not strictly ascending: {:?} is followed by {:?}. \
             FIRST_BYTE_INDEX buckets this table by first byte and assumes those runs are \
             contiguous, so that entry now resolves to None and renders the plains fallback. \
             Re-sort with `LC_ALL=C sort` — `_` is 0x5F, below every lowercase letter, so \
             'alphabetical ignoring underscores' is the wrong order.",
            first_disorder(&names).map(|i| names[i - 1]),
            first_disorder(&names).map(|i| names[i]),
        );
    }

    /// Control for the test above: the detector must actually fire. Both a swap
    /// and a duplicate are checked, because `>=` catches the second only if the
    /// comparison is not a bare `>`.
    #[test]
    fn the_ascending_check_finds_a_real_disorder() {
        assert_eq!(
            first_disorder(&["badlands", "beach", "bamboo_jungle"]),
            Some(2),
            "a swapped pair must be reported, or biome_effects_table_is_strictly_ascending \
             is vacuous"
        );
        assert_eq!(
            first_disorder(&["ocean", "ocean"]),
            Some(1),
            "a duplicate key must be reported too: binary_search_by picks an arbitrary one \
             of two equal entries"
        );
        assert_eq!(first_disorder(&["a", "b", "c"]), None);
        assert_eq!(first_disorder(&[]), None);
    }

    /// [`super::FIRST_BYTE_INDEX`] is built by a `const fn`, so the compiler
    /// checks nothing about it beyond bounds. Three properties, each of which a
    /// plausible off-by-one breaks differently: the ranges must be in bounds,
    /// every entry in a range must actually start with that byte, and the ranges
    /// must **cover all 66 entries** — the last is the one that catches a bucket
    /// that ends early, which is otherwise invisible until one biome stops
    /// resolving.
    #[test]
    fn the_first_byte_index_covers_every_entry_and_only_matching_ones() {
        let mut covered = 0usize;
        for (byte, &(start, end)) in super::FIRST_BYTE_INDEX.iter().enumerate() {
            let (start, end) = (start as usize, end as usize);
            assert!(
                start <= end && end <= super::BIOME_EFFECTS.len(),
                "byte {byte:#04X} indexes {start}..{end}, outside 0..{}",
                super::BIOME_EFFECTS.len()
            );
            for (name, _) in &super::BIOME_EFFECTS[start..end] {
                assert_eq!(
                    name.as_bytes()[0] as usize,
                    byte,
                    "{name} is in byte {byte:#04X}'s bucket but does not start with it"
                );
            }
            covered += end - start;
        }
        assert_eq!(
            covered,
            super::BIOME_EFFECTS.len(),
            "the first-byte buckets cover {covered} of {} entries. The uncovered ones resolve \
             to None and render the plains fallback",
            super::BIOME_EFFECTS.len()
        );
    }

    /// The reason the index is worth having at all, as a number rather than a
    /// claim: it must narrow the scan by roughly an order of magnitude. Compares
    /// are counted from the table itself, so this is arithmetic over outside data
    /// and not a re-measurement of the code.
    ///
    /// `DESIGN.md` §12.126 records the instruction cost this predicts.
    #[test]
    fn the_first_byte_index_narrows_the_average_scan_by_about_nine_times() {
        let n = super::BIOME_EFFECTS.len();
        // Position within the whole table, 1-based: what `find` used to pay.
        let flat: usize = (1..=n).sum();
        // Position within the entry's own bucket, 1-based: what it pays now.
        let bucketed: usize = super::FIRST_BYTE_INDEX
            .iter()
            .map(|&(start, end)| (1..=(end - start) as usize).sum::<usize>())
            .sum();
        let (flat, bucketed) = (flat as f64 / n as f64, bucketed as f64 / n as f64);
        assert!(
            (flat - 33.5).abs() < 0.01,
            "a 66-entry linear scan averages 33.5 compares, computed {flat:.2}"
        );
        assert!(
            bucketed < 4.5,
            "the bucketed scan averages {bucketed:.2} compares, not the ~3.8 the first-byte \
             distribution of the 66 biome names gives. Something has changed the bucketing, \
             and DESIGN.md §12.126's per-call instruction figures no longer follow"
        );
        assert!(
            flat / bucketed > 8.0,
            "the index narrows the average scan only {:.1}x ({flat:.2} -> {bucketed:.2} \
             compares). It was measured at 8.8x; below that the fixed cost of the index \
             lookup starts to matter and the shape control in \
             `client_chunk_cycles.rs` is the thing to re-read",
            flat / bucketed
        );
    }

    /// The property that actually matters, stated over the table itself rather
    /// than over a hand-written name list: every entry must be *findable*, and a
    /// name that sorts between two real entries must not be.
    ///
    /// `tests/tint.rs`'s `biome_effects_table_has_all_66_vanilla_biomes` checks
    /// the same round trip against a list transcribed from the jar, which is the
    /// stronger test of *contents*; this one is the stronger test of the *search*,
    /// because it cannot pass by both sides sharing an omission.
    #[test]
    fn every_table_entry_is_findable_and_a_between_name_is_not() {
        for (name, effects) in super::BIOME_EFFECTS {
            assert_eq!(
                super::biome_effects(name),
                Some(effects),
                "{name} is in the table but the search does not find it"
            );
            assert_eq!(
                super::biome_effects(&format!("minecraft:{name}")),
                Some(effects),
                "{name} does not resolve through the namespaced form"
            );
        }
        // Sorts strictly between "ocean" and "old_growth_birch_forest", so a
        // linear scan and a binary search must agree it is absent.
        assert!(super::biome_effects("oceanic").is_none());
        assert!(super::biome_effects("").is_none());
        assert!(super::biome_effects("zzz").is_none());
        assert!(super::biome_effects("aaa").is_none());
    }
}
