//! Bridge from `lodestone-world`'s real chunk storage into the mesher's
//! [`SectionView`] input.
//!
//! The mesher deliberately knows nothing about block-state ids, palettes, or the
//! block registry — it consumes [`Cell`]s. This module adapts a real
//! [`lodestone_world::ChunkSection`] (paletted `u32` block-state storage,
//! section-local `0..16` coordinates) into a [`SectionView`] by delegating the
//! "what does state id `N` look like?" question to a [`BlockClassifier`].
//!
//! That split is the important one: `lodestone-world` owns storage, a version
//! crate owns the state-id → appearance mapping, and the renderer owns geometry.
//! The classifier is the single seam where a real block registry (or a test
//! double) plugs in. Because [`ChunkSection::get_block`] is section-local
//! `0..16` — not world-Y like the column API — the adapter is a direct wrap with
//! no coordinate translation, and cross-section access stays the mesher's job
//! via [`SectionNeighborhood`](crate::section::SectionNeighborhood).

use lodestone_world::{ChunkSection, LightData, SectionLight as WorldLight};

use crate::section::{Cell, SectionView};

/// Resolves a block-state id into the renderer's [`Cell`] appearance.
///
/// This is the version-free seam between raw block-state ids and render data. A
/// protocol/version crate implements it against the real block registry; tests
/// implement it with a synthetic mapping. It is intentionally a pure function of
/// the id plus light — the renderer never reaches into block semantics itself.
pub trait BlockClassifier {
    /// The rendered [`Cell`] for `state_id`, given the block and sky light at
    /// that position (`0..=15`). Return a **lit but empty** cell for air /
    /// non-rendered states (`occludes: false`, `surface: None`, but with the
    /// real `block_light`/`sky_light`) — *not* [`Cell::EMPTY`].
    ///
    /// This matters: a block face samples its lighting from the neighbouring
    /// cell it faces into. If air is reported as the unlit [`Cell::EMPTY`]
    /// (`sky_light: 0`), every exposed block face bordering that air renders
    /// black. Air carries light in Minecraft; the classifier must preserve it.
    fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell;
}

/// A per-position light source for a section, in section-local `0..16`
/// coordinates. `lodestone-world` exposes light per column; a caller adapts it
/// to this for the section being meshed.
pub trait SectionLight {
    /// Block light `0..=15` at section-local `(x, y, z)`.
    fn block_light(&self, x: usize, y: usize, z: usize) -> u8;
    /// Sky light `0..=15` at section-local `(x, y, z)`.
    fn sky_light(&self, x: usize, y: usize, z: usize) -> u8;
}

/// A [`SectionLight`] that reports a constant sky light everywhere and no block
/// light.
///
/// This has two distinct uses that must not be confused:
///
/// * **Tests** legitimately want a constant, controllable light field.
/// * The **live mesher** uses it as a *pre-light bridge* ([`Self::pre_light_bridge`])
///   until `lodestone-world`'s `section_light` sampling and the matching
///   lock-free handle accessor are wired. This is a bridge, not real light, and
///   it is the dangerous kind: it renders *plausibly* (full-bright flat), so a
///   world stuck on it looks merely a little flat rather than obviously broken.
///   The bridge is therefore given a loud, greppable constructor and a canary
///   test (`pre_light_bridge_is_the_declared_full_bright_source` in
///   [`crate::mesher`]) so "real light never landed" surfaces as a named, tested
///   state rather than a silent default. See that module's light-seam contract.
///
/// Note the bridge must **not** be read as "sky is 15 everywhere" once real
/// light exists: an all-air nether section stores sky `0`, and defaulting it to
/// 15 is the too-bright-nether bug. The bridge is a stand-in for *absent* data,
/// not a claim about any dimension.
#[derive(Debug, Clone, Copy)]
pub struct UniformLight {
    /// Block light returned for every cell.
    pub block_light: u8,
    /// Sky light returned for every cell.
    pub sky_light: u8,
}

impl UniformLight {
    /// The **pre-light bridge**: full sky light (`15`), no block light (`0`).
    ///
    /// This is the *only* sanctioned way for the live mesher to obtain light
    /// before the real `section_light` seam is wired. It is named rather than
    /// spelled `UniformLight::default()` at the call site precisely so it cannot
    /// be mistaken for real light on a read of the meshing path: the word
    /// *bridge* is right there. When real per-section light lands (entering via
    /// the [`MeshJob`](crate::mesher::MeshJob) snapshot, not a shared parameter),
    /// this constructor's single live call site is the exact spot to replace,
    /// and the canary test guarding it must be updated deliberately.
    #[must_use]
    pub const fn pre_light_bridge() -> Self {
        Self {
            block_light: 0,
            sky_light: 15,
        }
    }
}

impl Default for UniformLight {
    /// Full sky light, no block light — identical to [`Self::pre_light_bridge`].
    /// Prefer the named constructor on the live path so the bridge is
    /// unmistakable; `default()` is a convenience for tests.
    fn default() -> Self {
        Self::pre_light_bridge()
    }
}

impl SectionLight for UniformLight {
    fn block_light(&self, _x: usize, _y: usize, _z: usize) -> u8 {
        self.block_light
    }
    fn sky_light(&self, _x: usize, _y: usize, _z: usize) -> u8 {
        self.sky_light
    }
}

/// How to resolve *absent* (`Missing`) sky light for a section — the one light
/// value `lodestone-world` deliberately cannot decide, because it depends on the
/// dimension (there is no sky light in the nether/end) and on whether the
/// section sits above the heightmap. The renderer's caller, which tracks the
/// dimension, chooses this policy; the mesher never coerces a default itself.
///
/// It applies **only** to `Missing` sky data. A section that *stores* a sky
/// value — including an all-air nether section stored as `0` — is real data and
/// is returned unchanged, so this can never manufacture the too-bright-nether
/// bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkyDefault {
    /// Absent sky light is full daylight (`15`): an overworld section above the
    /// heightmap that carried no light data of its own.
    Full,
    /// Absent sky light is `0`: the nether/end (no sky light at all), or a
    /// section below the heightmap. The nether-safe choice — absent nether sky
    /// must stay `0`, never default *up* to 15.
    None,
}

/// Adapts a [`lodestone_world::SectionLight`] snapshot into this crate's per-cell
/// [`SectionLight`] trait.
///
/// This is the render-side end of the light seam agreed with the light-engine
/// owner (see the [`mesher`](crate::mesher) light-seam contract). It forwards
/// resolved `u8` levels **verbatim** via the world snapshot's `sky_at`/`block_at`
/// — the nibble unpacking stays on the storage side, so the mesher's smooth-
/// lighting corner blend can never drift a nibble against storage — and it
/// applies the dimension-aware [`SkyDefault`] **only** to genuinely absent
/// (`Missing`) sky data. Block light is never defaulted up: absent block light
/// resolves to `0`, which is correct everywhere.
///
/// One adapter wraps one section; a mesher builds `section_count`-plus-neighbour
/// adapters and assembles them into the per-section light grid consumed by
/// [`SectionSnapshot::build_mesh`](crate::mesher::SectionSnapshot::build_mesh).
#[derive(Debug)]
pub struct WorldSectionLight<'a> {
    snapshot: &'a WorldLight,
    sky_default: SkyDefault,
}

impl<'a> WorldSectionLight<'a> {
    /// Wraps a `lodestone-world` light snapshot with the dimension's policy for
    /// absent sky light.
    #[must_use]
    pub fn new(snapshot: &'a WorldLight, sky_default: SkyDefault) -> Self {
        Self {
            snapshot,
            sky_default,
        }
    }
}

impl SectionLight for WorldSectionLight<'_> {
    fn block_light(&self, x: usize, y: usize, z: usize) -> u8 {
        self.snapshot.block_at(x, y, z)
    }

    fn sky_light(&self, x: usize, y: usize, z: usize) -> u8 {
        match self.snapshot.sky {
            // Absent data — the dimension policy decides, never the storage.
            LightData::Missing => match self.sky_default {
                SkyDefault::Full => 15,
                SkyDefault::None => 0,
            },
            // Stored data (including a nether section's Uniform(0)) is verbatim.
            _ => self.snapshot.sky_at(x, y, z),
        }
    }
}

/// Adapts a real [`ChunkSection`] into a [`SectionView`] using a
/// [`BlockClassifier`] and a [`SectionLight`].
///
/// Build one per section (including the 26 neighbours) and assemble them into a
/// [`SectionNeighborhood`](crate::section::SectionNeighborhood) for correct
/// boundary meshing.
#[derive(Debug)]
pub struct ChunkSectionView<'a, C: BlockClassifier, L: SectionLight> {
    section: &'a ChunkSection,
    classifier: &'a C,
    light: &'a L,
}

impl<'a, C: BlockClassifier, L: SectionLight> ChunkSectionView<'a, C, L> {
    /// Wraps a section with the classifier and light source used to resolve its
    /// cells.
    #[must_use]
    pub fn new(section: &'a ChunkSection, classifier: &'a C, light: &'a L) -> Self {
        Self {
            section,
            classifier,
            light,
        }
    }
}

impl<C: BlockClassifier, L: SectionLight> SectionView for ChunkSectionView<'_, C, L> {
    fn cell(&self, x: usize, y: usize, z: usize) -> Cell {
        let state = self.section.get_block(x, y, z);
        let bl = self.light.block_light(x, y, z);
        let sl = self.light.sky_light(x, y, z);
        self.classifier.classify(state, bl, sl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::{mesh_greedy, mesh_simple};
    use crate::section::{SectionNeighborhood, SpriteId};
    use lodestone_world::PaletteKind;

    const AIR: u32 = 0;
    const STONE: u32 = 1;

    /// Air is a lit-but-empty cell; every other id is a solid cube whose sprite
    /// is its id.
    #[derive(Debug)]
    struct SimpleClassifier;
    impl BlockClassifier for SimpleClassifier {
        fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell {
            if state_id == AIR {
                Cell {
                    occludes: false,
                    surface: None,
                    block_light,
                    sky_light,
                }
            } else {
                let mut c = Cell::solid(SpriteId(state_id as u16));
                c.block_light = block_light;
                c.sky_light = sky_light;
                c
            }
        }
    }

    fn stone_section() -> ChunkSection {
        // A real paletted section: solid stone floor (y=0), air above.
        let mut s = ChunkSection::new(PaletteKind::block_states(), PaletteKind::biomes(), AIR, 0);
        for x in 0..16 {
            for z in 0..16 {
                s.set_block(x, 0, z, STONE);
            }
        }
        s
    }

    #[test]
    fn adapter_reads_real_section_storage() {
        let s = stone_section();
        let light = UniformLight::default();
        let view = ChunkSectionView::new(&s, &SimpleClassifier, &light);
        // Floor cell is solid stone; the cell above is air.
        assert!(view.cell(3, 0, 5).occludes);
        assert_eq!(view.cell(3, 0, 5).surface.unwrap().sprites[0], SpriteId(1));
        assert!(!view.cell(3, 1, 5).occludes);
        assert!(view.cell(3, 1, 5).surface.is_none());
    }

    #[test]
    fn real_section_meshes_to_the_expected_floor() {
        let s = stone_section();
        let light = UniformLight::default();
        let view = ChunkSectionView::new(&s, &SimpleClassifier, &light);
        // Smooth lighting samples corner neighbours that cross the section
        // boundary, so a bare `centre_only` hood reads out-of-section cells as
        // unlit and fragments the merge at every edge. Surround the slab with
        // lit air (as the real pipeline's populated neighbourhood would) so the
        // boundary light is continuous and greedy can merge each plane.
        struct AirLit;
        impl SectionView for AirLit {
            fn cell(&self, _x: usize, _y: usize, _z: usize) -> Cell {
                Cell {
                    occludes: false,
                    surface: None,
                    block_light: 0,
                    sky_light: 15,
                }
            }
        }
        let air = AirLit;
        let mut hood = SectionNeighborhood::centre_only(&view);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if (dx, dy, dz) != (0, 0, 0) {
                        hood.set(dx, dy, dz, Some(&air));
                    }
                }
            }
        }
        // A 16×16 floor slab with lit-air neighbours: top + bottom + 4 sides.
        // Simple emits every exposed unit face; greedy merges each plane.
        let simple = mesh_simple(&hood);
        let greedy = mesh_greedy(&hood);
        assert!(simple.quad_count() > greedy.quad_count());
        // Greedy: top plane (1) + bottom plane (1) + 4 one-block-tall sides.
        assert_eq!(greedy.quad_count(), 6);
        assert!(!simple.vertices.is_empty());
    }

    #[test]
    fn light_flows_from_source_into_cells() {
        let s = stone_section();
        let light = UniformLight {
            block_light: 7,
            sky_light: 12,
        };
        let view = ChunkSectionView::new(&s, &SimpleClassifier, &light);
        let c = view.cell(0, 0, 0);
        assert_eq!(c.block_light, 7);
        assert_eq!(c.sky_light, 12);
    }

    #[test]
    fn world_section_light_forwards_levels_and_applies_explicit_sky_default() {
        // Resolved levels forward verbatim — the nibble unpacking stays on the
        // world side, so this adapter can never disagree with storage.
        let lit = WorldLight {
            sky: LightData::Uniform(12),
            block: LightData::Uniform(7),
        };
        let a = WorldSectionLight::new(&lit, SkyDefault::Full);
        assert_eq!(a.sky_light(1, 2, 3), 12);
        assert_eq!(a.block_light(1, 2, 3), 7);

        // Absent block light resolves to 0 everywhere and is never defaulted up.
        let missing_block = WorldLight {
            sky: LightData::Uniform(15),
            block: LightData::Missing,
        };
        assert_eq!(
            WorldSectionLight::new(&missing_block, SkyDefault::Full).block_light(0, 0, 0),
            0
        );

        // The dimension policy applies to *absent* (Missing) sky and nothing
        // else: overworld-above-heightmap -> 15, nether/end -> 0.
        let missing_sky = WorldLight {
            sky: LightData::Missing,
            block: LightData::Uniform(0),
        };
        assert_eq!(
            WorldSectionLight::new(&missing_sky, SkyDefault::Full).sky_light(5, 5, 5),
            15
        );
        assert_eq!(
            WorldSectionLight::new(&missing_sky, SkyDefault::None).sky_light(5, 5, 5),
            0
        );

        // A section that *stores* sky 0 (an all-air nether section) is real data,
        // not absence, so it reads 0 even under the Full policy — never defaulted
        // up to 15. This is the nether-safety invariant.
        let stored_dark_sky = WorldLight {
            sky: LightData::Uniform(0),
            block: LightData::Uniform(0),
        };
        assert_eq!(
            WorldSectionLight::new(&stored_dark_sky, SkyDefault::Full).sky_light(8, 8, 8),
            0
        );
    }
}
