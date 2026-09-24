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
//! which stays at the trait's default `0.0`. Porting vanilla's own biome-info
//! noise sampler
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

/// How many distinct biome names the per-instance name→effects memo holds.
///
/// Four, because that is what a blend box straddling a biome boundary needs:
/// vanilla's radius-2 box is 25 samples over a 5×5 column footprint, and a
/// four-way biome junction is the worst real case. A one-entry memo thrashes
/// there and degrades to the linear scan it exists to avoid.
const EFFECTS_MEMO: usize = 4;

/// A [`BiomeTint`] backed by a per-position biome **name** lookup (`F`) plus
/// [`biome_effects`]'s static vanilla table.
///
/// Generic over `F` rather than depending on `lodestone-world`: the real
/// lookup (`ChunkSection::biome_at_block` plus an id→name table) lives with
/// whoever owns the world data — `crates/lodestone-shell/src/mesher.rs`'s
/// `SnapshotModelView`/`SnapshotFluidView` today.
///
/// # The memo, and why it is here rather than in `biome_effects`
///
/// [`biome_effects`] is a lookup in a 66-entry `(&str, BiomeEffects)` table, and
/// this type calls it once per [`BiomeTint`] method call — which
/// [`resolve_blended_tint`] makes 25 times per tinted quad (vanilla's radius-2
/// blend box), and up to four times *per sample* for grass (`grass_override` +
/// `temperature` + `downfall` + `grass_modifier`). When that lookup was a linear
/// `find` with a string compare per entry, one `water_tint_at` cost **6,263
/// instructions, of which 97.8% was `biome_effects`** — 46% of `mesh_fluids`'s
/// entire per-cell cost, and a term separate from the per-cell virtual-call
/// cost the fluid mesher otherwise pays (`DESIGN.md` §12.124).
///
/// A blend box covers 25 columns and terrain has *one or two* biomes there, so
/// the same handful of names is re-looked-up dozens of times. This memo keys on
/// the `&'static str`'s **data pointer and length**, not its contents: names come
/// from static tables, so the same biome yields the same pointer. A pointer miss
/// on an equal string is merely slow, never wrong — the fallback is the real
/// [`biome_effects`] call.
///
/// [`biome_effects`] now narrows its scan with a compile-time first-byte index
/// (3.79 compares on an average hit rather than 33.5, and usually one or none on a
/// miss), which fixed every caller rather than this one — so the memo is no longer
/// load-bearing for the *scan*, only for the `strip_prefix` + call + branch around
/// it. It is kept because it still measures as a win and because it is what makes
/// a boundary cell cheap. §12.128 has what each is worth on its own, including why
/// the `binary_search_by` this comment used to recommend measured **worse**.
pub struct NamedBiomeTint<F> {
    biome_name_at: F,
    /// `(ptr, len, effects)` for the last few resolved names. `Cell` rather than
    /// `RefCell`: every field is `Copy`, so there is nothing to borrow. The
    /// pointer is held as a `usize` and never dereferenced — a raw pointer field
    /// would make this type `!Send`, and it is built on rayon mesh workers.
    memo: std::cell::Cell<[Option<(usize, usize, &'static BiomeEffects)>; EFFECTS_MEMO]>,
    /// Where the next miss writes. Round-robin, so a boundary that cycles
    /// through `EFFECTS_MEMO` names keeps hitting.
    next: std::cell::Cell<usize>,
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
        Self {
            biome_name_at,
            memo: std::cell::Cell::new([None; EFFECTS_MEMO]),
            next: std::cell::Cell::new(0),
        }
    }

    fn effects(&self, pos: BlockPos) -> &'static BiomeEffects {
        let Some(name) = (self.biome_name_at)(pos) else {
            return &PLAINS_FALLBACK;
        };
        let key = (name.as_ptr() as usize, name.len());
        let memo = self.memo.get();
        for slot in memo.iter().flatten() {
            if slot.0 == key.0 && slot.1 == key.1 {
                return slot.2;
            }
        }
        // Miss: the real lookup, exactly as before. An unresolvable name is
        // deliberately NOT memoised — it costs one scan and would otherwise
        // evict a name that is paying for itself.
        let Some(effects) = biome_effects(name) else {
            return &PLAINS_FALLBACK;
        };
        let mut memo = memo;
        let i = self.next.get();
        memo[i] = Some((key.0, key.1, effects));
        self.memo.set(memo);
        self.next.set((i + 1) % EFFECTS_MEMO);
        effects
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
/// * one point: `Colormaps::resolve` is vanilla's own per-kind colour-resolver
///   callback
///   (vanilla's own per-biome grass/foliage/dry-foliage/water colour
///   accessors) — the colormap sample (or override) plus the grass
///   modifier, all evaluated at *that* sample's own biome;
/// * the box: [`blend_box`] wraps it exactly like vanilla's own client-level
///   block-tint calculation (client-level's own decompiled source) wraps the resolver —
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
    if !is_blended_kind(kind) {
        return None;
    }
    Some(blend_box(x, z, radius, |sx, sz| {
        colormaps
            .resolve(kind, biome, BlockPos::new(sx, y, sz))
            .unwrap_or(0)
    }))
}

/// Whether `kind` is one of the four position-dependent kinds a blend applies to.
///
/// Shared by [`resolve_blended_tint`] and [`BlendedTintCursor::resolve`] rather
/// than written twice, so the two cannot drift into disagreeing about which kinds
/// return `None` — a disagreement that would show up as a *tinted* quad on one
/// path and an untinted one on the other, with both paths individually green.
#[must_use]
pub const fn is_blended_kind(kind: TintKind) -> bool {
    matches!(
        kind,
        TintKind::Grass | TintKind::Foliage | TintKind::DryFoliage | TintKind::Water
    )
}

/// [`resolve_blended_tint`] with the blend box **shared between adjacent cells of
/// the same row** — bit-identical output, ~5× fewer `Colormaps::resolve` samples.
///
/// # Why this exists
///
/// Even with the per-cell virtual-call cost reduced elsewhere, the biome tint
/// is still ~63% of `mesh_fluids`'s per-cell cost (`DESIGN.md` §12.124), and
/// almost all of that is
/// the 25 samples vanilla's radius-2 box takes per tinted quad. Adjacent cells
/// share 20 of those 25 columns; [`lodestone_assets::tint::BlendRowCursor`] turns
/// that into a sliding per-channel sum, and its docs carry the bit-exactness
/// argument. This type adds the part the row cursor cannot know: a blend also
/// depends on the [`TintKind`] and on the `y` every sample is taken at, neither of
/// which is `(x, z)`.
///
/// # How to use it, and the one way to get it wrong
///
/// Hold one per mesh pass (`mesher.rs`'s `SnapshotFluidView`/`SnapshotModelView`
/// keep one in a `RefCell`) and call [`resolve`](Self::resolve) in place of
/// [`resolve_blended_tint`]. **The cursor caches sampled colours, so it is only
/// correct while the world it samples is unchanging** — that holds inside one
/// `mesh_fluids`/`mesh_models` call over an immutable snapshot, and it is why this
/// is not a global cache. It also assumes the `biome`/`colormaps` handed to
/// successive calls answer identically; passing a *different* biome source with
/// the same `(kind, y, z, x)` would read the previous one's columns. Both are
/// caller obligations that no signature can express, which is why the identity
/// gate drives it through the real `mesh_fluids`/`mesh_models` loops rather than
/// calling it directly.
///
/// A mismatch on `kind` or `y` invalidates the window and costs one full rebuild,
/// i.e. exactly [`resolve_blended_tint`] — so a caller whose access pattern is
/// hostile pays nothing extra beyond the key comparison.
#[derive(Debug)]
pub struct BlendedTintCursor {
    row: lodestone_assets::tint::BlendRowCursor,
    /// The `(kind, y)` the loaded window belongs to. `BlendRowCursor` keys itself
    /// on `(x, z)`; these two are the rest of what `Colormaps::resolve` reads, and
    /// are invisible to it.
    key: Option<(TintKind, i32)>,
}

impl BlendedTintCursor {
    /// A cursor for a fixed blend `radius` — [`BLEND_RADIUS`] for everything in
    /// this client today. Fixed at construction, so two radii cannot end up
    /// mixed into one window.
    #[must_use]
    pub fn new(radius: i32) -> Self {
        Self {
            row: lodestone_assets::tint::BlendRowCursor::new(radius),
            key: None,
        }
    }

    /// The blend radius this cursor was built with.
    #[must_use]
    pub fn radius(&self) -> i32 {
        self.row.radius()
    }

    /// Bit-identical to
    /// `resolve_blended_tint(kind, colormaps, biome, self.radius(), x, y, z)`.
    pub fn resolve(
        &mut self,
        kind: TintKind,
        colormaps: &Colormaps,
        biome: &dyn BiomeTint,
        x: i32,
        y: i32,
        z: i32,
    ) -> Option<Rgb> {
        if !is_blended_kind(kind) {
            return None;
        }
        if self.key != Some((kind, y)) {
            self.key = Some((kind, y));
            self.row.invalidate();
        }
        Some(self.row.blend(x, z, |sx, sz| {
            colormaps
                .resolve(kind, biome, BlockPos::new(sx, y, sz))
                .unwrap_or(0)
        }))
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

    /// The memo must be **transparent**, including when more distinct biomes
    /// pass through it than it has slots — the case where a wrong eviction
    /// returns another biome's effects.
    ///
    /// Eight names cycle through a four-slot memo, so every lookup after the
    /// first four evicts something, and the walk is repeated twice so the second
    /// pass reads whatever the first left behind. The expected water colour of
    /// each name comes from `biome_effects` **directly**, not from this type, so
    /// the expectation does not share the mechanism under test.
    #[test]
    fn the_effects_memo_is_transparent_under_eviction() {
        // Eight names with eight *distinct* water colours, so a wrong hit shows
        // up as a wrong colour rather than an accidental match.
        const NAMES: [&str; 8] = [
            "minecraft:swamp",
            "minecraft:cold_ocean",
            "minecraft:warm_ocean",
            "minecraft:frozen_ocean",
            "minecraft:meadow",
            "minecraft:cherry_grove",
            "minecraft:pale_garden",
            "minecraft:mangrove_swamp",
        ];
        assert!(
            NAMES.len() > EFFECTS_MEMO,
            "the fixture must overflow the memo, or eviction is never exercised"
        );
        let distinct: std::collections::BTreeSet<Rgb> = NAMES
            .iter()
            .map(|n| biome_effects(n).expect("a known biome").water_color)
            .collect();
        assert_eq!(
            distinct.len(),
            NAMES.len(),
            "the fixture's water colours must all differ, or a wrong hit could pass"
        );

        // `x` picks the name; the closure is position-dependent, exactly as the
        // real snapshot lookup is.
        let tint = NamedBiomeTint::new(|pos: BlockPos| {
            Some(NAMES[pos.x.rem_euclid(NAMES.len() as i32) as usize])
        });
        for _pass in 0..2 {
            for (i, name) in NAMES.iter().enumerate() {
                let want = biome_effects(name).expect("a known biome");
                let pos = BlockPos::new(i as i32, 64, 0);
                assert_eq!(
                    tint.water_color(pos),
                    want.water_color,
                    "{name} resolved to the wrong water colour through the memo"
                );
                assert_eq!(tint.temperature(pos), want.temperature, "{name} temperature");
                assert_eq!(tint.grass_modifier(pos), want.grass_modifier, "{name} modifier");
            }
        }
    }

    /// Control: the memo must actually *hit*, or it is dead weight and the
    /// measured saving in `DESIGN.md` §12.124 could not have come from here.
    ///
    /// Counts calls to the name closure and to a hand-rolled resolver standing
    /// in for the scan: a radius-2 blend over a single biome is 25 name lookups,
    /// and exactly **one** of them may reach the table.
    #[test]
    fn the_effects_memo_reaches_the_table_once_per_blend_box() {
        use std::cell::Cell;
        let name_calls = Cell::new(0usize);
        let tint = NamedBiomeTint::new(|_pos: BlockPos| {
            name_calls.set(name_calls.get() + 1);
            Some("minecraft:swamp")
        });
        let colormaps = tiny_colormaps();
        let before = tint.memo.get().iter().flatten().count();
        assert_eq!(before, 0, "the memo must start empty");
        let c = resolve_blended_tint(TintKind::Water, &colormaps, &tint, 2, 0, 64, 0)
            .expect("water blends");
        assert_eq!(c, 0x617B64, "the blended swamp water colour must be unchanged");
        assert_eq!(
            name_calls.get(),
            25,
            "a radius-2 blend box is 25 samples; the memo must not change how many \
             positions are consulted, only how many reach the 66-entry scan"
        );
        assert_eq!(
            tint.memo.get().iter().flatten().count(),
            1,
            "25 samples of one biome must leave exactly one memo entry — more means the \
             memo is not being consulted and every sample re-scanned the table"
        );
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
