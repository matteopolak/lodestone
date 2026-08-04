//! Real, per-position biome tint: wires [`lodestone_assets::tint`]'s
//! `BiomeTint`/`Colormaps`/`blend_box` seam into something a
//! [`crate::models::ModelSectionView`]/[`crate::models::FluidSectionView`]
//! implementor can call per quad.
//!
//! # Why this crate, not `lodestone-world`/`lodestone-shell`
//!
//! `lodestone_assets::tint`'s own module docs already explain the split:
//! *which resolver a block uses* is version-crate/render knowledge
//! ([`vanilla_tint_kind`](lodestone_assets::tint::vanilla_tint_kind)), while
//! *biome climate at a position* is a world/render concern, expressed as the
//! [`BiomeTint`](lodestone_assets::tint::BiomeTint) trait. This module is the
//! second half: it turns "what biome is at this block" (a caller-supplied
//! closure — this crate never touches `lodestone-world`) plus the static
//! vanilla biome table into a real `BiomeTint`, and wraps `Colormaps::resolve`
//! in vanilla's own box-blend kernel.
//!
//! # What is and isn't ported
//!
//! Every field of [`lodestone_assets::tint::BiomeEffects`] is used —
//! temperature, downfall, the three colormap overrides, the water colour, the
//! grass modifier — **except** the swamp modifier's noise term
//! ([`BiomeTint::grass_modifier_noise`](lodestone_assets::tint::BiomeTint::grass_modifier_noise)),
//! which stays at the trait's default `0.0`. Porting `Biome.BIOME_INFO_NOISE`
//! (a Perlin sampler) would pull a worldgen-noise dependency into a render
//! crate for one biome's two-tone patchiness (`swamp`/`mangrove_swamp`'s dark
//! patches — `GrassColorModifier::Swamp`'s `< -0.1` branch, see
//! `lodestone_assets::tint::GrassColorModifier::modify`); `0.0` always takes
//! the `>= -0.1` arm, so those two biomes render a uniform `0x6A7039`/
//! `0x8DB127`-derived green rather than vanilla's mottled one. Every other
//! biome (64 of 66) is unaffected. Worth porting once a shared noise crate
//! exists; not attempted here.
//!
//! # The id→name gap
//!
//! This module resolves a *name* (`"minecraft:swamp"`) to
//! [`lodestone_assets::tint::BiomeEffects`]; it does not resolve a *wire*
//! biome id (the `u32` [`lodestone_world::ChunkSection::biome_at_block`]
//! stores) to that name. That mapping is per-connection (a server's
//! `registry_data` sync order), which this crate has no seam for yet — see
//! `crates/lodestone-shell/src/mesher.rs`'s `FALLBACK_BIOME_NAMES` for the
//! current stand-in and its documented limits.

use lodestone_assets::tint::{
    BiomeEffects, BiomeTint, Colormaps, GrassColorModifier, Rgb, TintKind, biome_effects,
    blend_box,
};
use lodestone_model::BlockPos;

/// The climate/effects a position falls back to when its biome name doesn't
/// resolve — an id past the known set, or a caller with no biome answer at
/// all yet. Matches vanilla plains' own `effects.water_color` (`0x3F76E4`)
/// and `temperature`/`downfall` (`0.8`/`0.4`), so an unresolved position
/// renders exactly the pre-existing plains-default look (see
/// `crate::block_resolver::{PLAINS_TEMPERATURE, PLAINS_DOWNFALL}`) rather
/// than an arbitrary or jarring colour.
const PLAINS_FALLBACK: BiomeEffects = BiomeEffects {
    temperature: 0.8,
    downfall: 0.4,
    water_color: 0x003F_76E4,
    grass_color: None,
    foliage_color: None,
    dry_foliage_color: None,
    grass_modifier: GrassColorModifier::None,
};

/// A [`BiomeTint`] backed by a per-position biome **name** lookup (`F`) plus
/// [`biome_effects`]'s static vanilla table.
///
/// Generic over `F` rather than depending on `lodestone-world`: the real
/// lookup (`ChunkSection::biome_at_block` plus an id→name table) lives with
/// whoever owns the world data — `crates/lodestone-shell/src/mesher.rs`'s
/// `SnapshotModelView`/`SnapshotFluidView` today.
pub struct NamedBiomeTint<F> {
    biome_name_at: F,
}

impl<F> std::fmt::Debug for NamedBiomeTint<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedBiomeTint").finish_non_exhaustive()
    }
}

impl<F: Fn(BlockPos) -> Option<&'static str>> NamedBiomeTint<F> {
    /// Wraps a per-position biome-name closure. `biome_name_at` should return
    /// e.g. `"minecraft:swamp"` or `"swamp"` (both accepted, see
    /// [`biome_effects`]) — `None` falls back to [`PLAINS_FALLBACK`].
    #[must_use]
    pub fn new(biome_name_at: F) -> Self {
        Self { biome_name_at }
    }

    fn effects(&self, pos: BlockPos) -> &'static BiomeEffects {
        (self.biome_name_at)(pos)
            .and_then(biome_effects)
            .unwrap_or(&PLAINS_FALLBACK)
    }
}

impl<F: Fn(BlockPos) -> Option<&'static str>> BiomeTint for NamedBiomeTint<F> {
    fn temperature(&self, pos: BlockPos) -> f32 {
        self.effects(pos).temperature
    }

    fn downfall(&self, pos: BlockPos) -> f32 {
        self.effects(pos).downfall
    }

    fn water_color(&self, pos: BlockPos) -> Rgb {
        self.effects(pos).water_color
    }

    fn grass_override(&self, pos: BlockPos) -> Option<Rgb> {
        self.effects(pos).grass_color
    }

    fn foliage_override(&self, pos: BlockPos) -> Option<Rgb> {
        self.effects(pos).foliage_color
    }

    fn dry_foliage_override(&self, pos: BlockPos) -> Option<Rgb> {
        self.effects(pos).dry_foliage_color
    }

    fn grass_modifier(&self, pos: BlockPos) -> GrassColorModifier {
        self.effects(pos).grass_modifier
    }

    // `grass_modifier_noise` stays at the trait default (`0.0`) — see the
    // module docs' "What is and isn't ported".
}

/// Resolves the **real, vanilla-blended** colour for a biome-dependent `kind`
/// at world position `(x, y, z)`.
///
/// Mirrors vanilla's own two-layer split exactly:
/// * one point: `Colormaps::resolve` is `ColorResolver.getColor`
///   (`Biome.getGrassColor`/`getFoliageColor`/`getDryFoliageColor`/
///   `getWaterColor`) — the colormap sample (or override) plus the grass
///   modifier, all evaluated at *that* sample's own biome;
/// * the box: [`blend_box`] wraps it exactly like `ClientLevel.
///   calculateBlockTint` (`ClientLevel.java:1012-1034`) wraps the resolver —
///   a `(2*radius+1)²` average of the *resolved* colour, sampled at fixed `y`
///   across `x`±radius, `z`±radius, with vanilla's own per-channel integer
///   (floor) division. `radius` should be
///   [`DEFAULT_BLEND_RADIUS`] unless a caller has an actual video-settings
///   seam (this client doesn't yet).
///
/// Returns `None` for [`TintKind::None`]/[`TintKind::Constant`]/
/// [`TintKind::RedstonePower`] — kinds that are not position-dependent at
/// all, and have nothing here to blend; see
/// `crate::block_models::biome_tint_slot` for the reserved-slot mechanism
/// those three keep using instead.
#[must_use]
pub fn resolve_blended_tint(
    kind: TintKind,
    colormaps: &Colormaps,
    biome: &dyn BiomeTint,
    radius: i32,
    x: i32,
    y: i32,
    z: i32,
) -> Option<Rgb> {
    match kind {
        TintKind::None | TintKind::Constant(_) | TintKind::RedstonePower(_) => None,
        TintKind::Grass | TintKind::Foliage | TintKind::DryFoliage | TintKind::Water => {
            Some(blend_box(x, z, radius, |sx, sz| {
                colormaps
                    .resolve(kind, biome, BlockPos::new(sx, y, sz))
                    .unwrap_or(0)
            }))
        }
    }
}

/// Unpacks a `0xRRGGBB` [`Rgb`] into the `[r, g, b]` bytes
/// [`crate::models::ModelSectionView::biome_tint_at`]/
/// [`crate::models::FluidSectionView::water_tint_at`] return.
#[must_use]
pub const fn rgb_to_bytes(rgb: Rgb) -> [u8; 3] {
    [
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
    ]
}

/// Vanilla's default biome-blend radius, re-exported so a caller doesn't need
/// a second `lodestone_assets` import just for this constant.
pub use lodestone_assets::tint::DEFAULT_BLEND_RADIUS as BLEND_RADIUS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_biome_tint_reads_the_real_table() {
        let tint = NamedBiomeTint::new(|_pos| Some("minecraft:swamp"));
        let pos = BlockPos::new(0, 64, 0);
        assert_eq!(tint.water_color(pos), 0x617B64);
        assert_eq!(tint.foliage_override(pos), Some(0x6A7039));
        assert_eq!(tint.dry_foliage_override(pos), Some(0x7B5334));
        assert_eq!(tint.grass_override(pos), None);
        assert_eq!(tint.grass_modifier(pos), GrassColorModifier::Swamp);
        assert_eq!(tint.temperature(pos), 0.8);
        assert_eq!(tint.downfall(pos), 0.9);
    }

    #[test]
    fn named_biome_tint_falls_back_to_plains_for_unknown_or_absent() {
        for name in [None, Some("minecraft:not_a_biome")] {
            let tint = NamedBiomeTint::new(move |_pos| name);
            let pos = BlockPos::new(1, 2, 3);
            assert_eq!(tint.water_color(pos), 0x3F76E4);
            assert_eq!(tint.temperature(pos), 0.8);
            assert_eq!(tint.downfall(pos), 0.4);
            assert_eq!(tint.grass_modifier(pos), GrassColorModifier::None);
        }
    }

    #[test]
    fn named_biome_tint_varies_with_position() {
        // Two different biomes on either side of x = 0 — proves the closure's
        // position argument, not just its return value, is actually consulted.
        let tint = NamedBiomeTint::new(|pos: BlockPos| {
            if pos.x < 0 {
                Some("minecraft:desert")
            } else {
                Some("minecraft:swamp")
            }
        });
        assert_eq!(tint.water_color(BlockPos::new(-5, 64, 0)), 0x3F76E4);
        assert_eq!(tint.water_color(BlockPos::new(5, 64, 0)), 0x617B64);
    }

    #[test]
    fn resolve_blended_tint_none_for_position_independent_kinds() {
        let colormaps = tiny_colormaps();
        let biome = NamedBiomeTint::new(|_| Some("minecraft:plains"));
        for kind in [
            TintKind::None,
            TintKind::Constant(0x123456),
            TintKind::RedstonePower(7),
        ] {
            assert_eq!(
                resolve_blended_tint(kind, &colormaps, &biome, 2, 0, 64, 0),
                None
            );
        }
    }

    #[test]
    fn resolve_blended_tint_water_is_uniform_water_color_away_from_boundary() {
        // Vanilla's water resolver ignores x/z entirely (no colormap sample),
        // so blending a uniform biome must be the identity: the swamp water
        // colour exactly, at every radius.
        let colormaps = tiny_colormaps();
        let biome = NamedBiomeTint::new(|_| Some("minecraft:swamp"));
        let c = resolve_blended_tint(TintKind::Water, &colormaps, &biome, 2, 100, 64, -50)
            .expect("water blends");
        assert_eq!(c, 0x617B64);
    }

    #[test]
    fn resolve_blended_tint_grass_blends_across_a_biome_boundary() {
        // A hard x=0 boundary between plains (default colormap green) and
        // swamp (uniform 0x6A7039, since GrassColorModifier::Swamp's noise
        // default lands >= -0.1). Sampled a few blocks into the plains side
        // at the default radius, the result must sit strictly between the two
        // pure colours on the green channel — proof of a real blend, not a
        // per-block snap to one side or the other.
        let colormaps = tiny_colormaps();
        let biome = NamedBiomeTint::new(|pos: BlockPos| {
            if pos.x < 0 {
                Some("minecraft:plains")
            } else {
                Some("minecraft:swamp")
            }
        });
        let pure_plains =
            resolve_blended_tint(TintKind::Grass, &colormaps, &biome, 0, -100, 64, 0)
                .expect("plains grass resolves");
        let pure_swamp = resolve_blended_tint(TintKind::Grass, &colormaps, &biome, 0, 100, 64, 0)
            .expect("swamp grass resolves");
        let near_boundary =
            resolve_blended_tint(TintKind::Grass, &colormaps, &biome, 2, -1, 64, 0)
                .expect("near-boundary grass resolves");
        let g = |c: Rgb| (c >> 8) & 0xFF;
        assert_ne!(pure_plains, pure_swamp, "fixture must actually differ");
        let (lo, hi) = if g(pure_plains) < g(pure_swamp) {
            (g(pure_plains), g(pure_swamp))
        } else {
            (g(pure_swamp), g(pure_plains))
        };
        assert!(
            g(near_boundary) > lo && g(near_boundary) < hi,
            "blended green {} must sit strictly between the two pure biomes' green ({lo}..{hi})",
            g(near_boundary)
        );
    }

    /// A minimal but real [`Colormaps`]: tiny synthetic grass/foliage/
    /// dry-foliage PNGs decoded through the real [`lodestone_assets::tint::
    /// Colormap::from_image`] path, not a hand-built stand-in — so these
    /// tests exercise the same sampling code the real 256×256 vanilla PNGs
    /// go through.
    fn tiny_colormaps() -> Colormaps {
        use lodestone_assets::Image;
        use lodestone_assets::tint::Colormap;

        let solid = |rgb: u32| -> Colormap {
            let r = ((rgb >> 16) & 0xFF) as u8;
            let g = ((rgb >> 8) & 0xFF) as u8;
            let b = (rgb & 0xFF) as u8;
            let img = Image {
                width: 1,
                height: 1,
                rgba: vec![r, g, b, 255],
            };
            Colormap::from_image(&img, rgb).expect("1x1 colormap")
        };
        // A 1x1 map always samples its one pixel regardless of temp/downfall
        // (any index falls back to `default`, which is the same colour) — a
        // deliberately uniform stand-in for vanilla's real gradient PNG, good
        // enough to prove the *blend* math without needing the real asset.
        Colormaps {
            grass: solid(0x91BD59),
            foliage: solid(0x77AB2F),
            dry_foliage: solid(lodestone_assets::tint::colors::DRY_FOLIAGE_DEFAULT),
        }
    }
}
