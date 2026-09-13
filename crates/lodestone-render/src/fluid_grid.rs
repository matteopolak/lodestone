//! A padded, precomputed neighbourhood grid for the fluid mesher.
//!
//! # What it is
//!
//! An 18×18×18 array of packed per-cell facts — fluid kind, fluid amount,
//! `falling`, "occludes a fluid face", "takes the water overlay sprite" — built
//! **once** per section so [`crate::models::mesh_fluids`]'s inner loop reads an
//! array index instead of calling back through
//! [`FluidSectionView`](crate::models::FluidSectionView) for every probe.
//!
//! # Why
//!
//! `mesh_fluids` asks its view about the neighbourhood on the order of **fifty
//! times per fluid cell**: twelve `neighbor_height_at` corner probes (two
//! `fluid_at`s each), four `flow_neighbor_at`s (three probes each), six `same`
//! checks, the up-occlusion test, and — for a surface cell — nine more inside
//! `should_render_backward_up_face`. Every one of those re-decoded the
//! coordinate from scratch: three `split16`s, three range checks, a
//! snapshot-slot index and a `PalettedContainer::get` bit-unpack, then a `Vec`
//! lookup in `BlockModels`. The same neighbour cell was re-resolved many times
//! within one cell and again by every adjacent cell. Measured at
//! **13,709 instructions per fluid cell** (`DESIGN.md` §12.120); the grid turns
//! the whole neighbourhood walk into 11.6 KiB of L1-resident array reads.
//!
//! # How it works
//!
//! The grid spans `-1..=16` on each axis, which is *exactly* the reach of
//! `mesh_fluids` from an in-section cell: every probe is `±1` in `x`/`z` and
//! `-1..=+1` in `y`, and the deepest nesting (`neighbor_height_at` at
//! `(x±1, y, z±1)` reading its own `y + 1`) still lands inside that box. A
//! probe outside is a bug, and `debug_assert`s in [`FluidGrid::get`] say so.
//!
//! Filling is two passes:
//!
//! 1. the interior `0..16` cube, one [`FluidSectionView::cell_at`] call per
//!    cell, recording the bounding box of the cells that actually carry fluid;
//! 2. the shell, but **only over that bounding box grown by one** — so a
//!    section with a puddle in one corner fills a handful of shell cells rather
//!    than all 1,736 of them.
//!
//! A section with no fluid at all stops after pass 1 and
//! [`FluidGrid::any_fluid`] reports `false`, which is also the "contains no
//! fluid" precheck the issue proposed — free, as a by-product, rather than as
//! a separate palette scan.
//!
//! # How to change it, and the gotchas
//!
//! * **The pack is exactly 16 bits and every bit is spoken for.** `falling` is
//!   in there even though `mesh_fluids` never reads it, because the point of
//!   the grid is that [`PackedCell::fluid`] reconstructs a *whole*
//!   [`FluidCell`] — a future consumer that needs `falling` must not silently
//!   get `false`. If you need a fifth fact, widen `PackedCell` rather than
//!   stealing the bit.
//! * **`partial_occluder_y_range_at` is deliberately NOT in the grid.** It
//!   returns two `f32`s (so it does not pack), and it is consulted at most four
//!   times per *surface* cell — a few percent of an ocean section — while
//!   filling it would cost an `outline_boxes` lookup for all 5,832 cells. It
//!   stays a live call on the view.
//! * **`cell_at`'s default composes the three existing accessors**, so every
//!   existing `FluidSectionView` implementor keeps compiling and answers
//!   identically. Overriding it is purely a cost optimisation for a view whose
//!   three accessors share a block-state read.
//! * The grid asks each cell **once**, where the old code asked lazily. So a
//!   view's `occludes_at`/`overlay_at` is now called for cells where the old
//!   `match` short-circuited past it. Every implementor in this workspace is a
//!   pure function of the coordinate, so this is invisible — but a view with
//!   side effects or a coordinate-dependent panic would notice.
//!
//! # Dependencies
//!
//! [`crate::models::FluidSectionView`] for the fill, and
//! [`crate::block_models::FluidCell`]/[`lodestone_assets::fluid::FluidState`]
//! for what a cell means.

use lodestone_assets::fluid::FluidState;

use crate::block_models::{FluidCell, FluidKind};
use crate::models::FluidSectionView;
use crate::section::SECTION_SIZE;

/// One padded cell either side of the section, on every axis.
const PAD: i32 = 1;

/// The grid's edge length: the 16-cell section plus one padding cell either
/// side. Derived from [`SECTION_SIZE`] rather than written as `18`, so a
/// hypothetical non-16 section size cannot silently mis-size the grid.
pub const GRID_DIM: usize = SECTION_SIZE + 2 * PAD as usize;

/// Total cells in the padded grid.
pub const GRID_CELLS: usize = GRID_DIM * GRID_DIM * GRID_DIM;

/// The linear index of padded coordinate `(x, y, z)`, each in `-1..=16`.
///
/// `y`-major then `z` then `x`, matching `mesh_fluids`'s own loop nesting so
/// the inner `x` walk is contiguous.
#[inline]
#[must_use]
fn index(x: i32, y: i32, z: i32) -> usize {
    let d = GRID_DIM;
    (((y + PAD) as usize) * d + ((z + PAD) as usize)) * d + ((x + PAD) as usize)
}

/// Everything [`crate::models::mesh_fluids`] needs to know about one cell,
/// resolved in a single [`FluidSectionView::cell_at`] call.
///
/// Deliberately *not* `partial_occluder_y_range_at` — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FluidNeighborCell {
    /// The fluid occupying the cell, or `None`.
    pub fluid: Option<FluidCell>,
    /// Whether the cell fully occludes an adjacent fluid face.
    pub occludes: bool,
    /// Whether a fluid side face touching this cell takes the `water_overlay`
    /// sprite.
    pub overlay: bool,
}

/// A [`FluidNeighborCell`] squeezed into 16 bits.
///
/// | bits | meaning |
/// |---|---|
/// | 0–1 | fluid kind: `0` none, [`KIND_WATER`], [`KIND_LAVA`] |
/// | 2–5 | [`FluidState::amount`], `1..=8` |
/// | 6 | [`FluidState::falling`] |
/// | 7 | occludes |
/// | 8 | overlay |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PackedCell(u16);

/// The `kind` bits of a cell carrying no fluid. Distinct from every real kind,
/// so `kind_bits() == centre_kind_bits` *is* the `Some(f) if f.kind == kind`
/// test — an empty cell can never match, because the centre cell of the mesh
/// loop always carries a fluid.
pub const KIND_NONE: u16 = 0;
/// The `kind` bits for [`FluidKind::Water`].
pub const KIND_WATER: u16 = 1;
/// The `kind` bits for [`FluidKind::Lava`].
pub const KIND_LAVA: u16 = 2;

const KIND_MASK: u16 = 0b11;
const AMOUNT_SHIFT: u32 = 2;
const AMOUNT_MASK: u16 = 0b1111;
const FALLING_BIT: u16 = 1 << 6;
const OCCLUDES_BIT: u16 = 1 << 7;
const OVERLAY_BIT: u16 = 1 << 8;

impl PackedCell {
    /// Air: no fluid, no occlusion, no overlay — the fill value for a cell the
    /// shell pass never visits.
    pub const EMPTY: Self = Self(0);

    /// Packs a [`FluidNeighborCell`].
    #[must_use]
    pub fn pack(cell: FluidNeighborCell) -> Self {
        let mut bits = match cell.fluid {
            None => KIND_NONE,
            Some(f) => {
                let kind = match f.kind {
                    FluidKind::Water => KIND_WATER,
                    FluidKind::Lava => KIND_LAVA,
                };
                // `amount` is `1..=8` by construction (vanilla's own fluid
                // level maps onto that range), so it always fits four bits. A value
                // out of range would silently truncate, so clamp it loudly in
                // debug and saturate in release rather than aliasing onto a
                // different height.
                debug_assert!(
                    (1..=8).contains(&f.state.amount),
                    "fluid amount {} outside 1..=8 would not fit the grid's four bits",
                    f.state.amount
                );
                let amount = u16::from(f.state.amount.min(AMOUNT_MASK as u8));
                kind | (amount << AMOUNT_SHIFT) | if f.state.falling { FALLING_BIT } else { 0 }
            }
        };
        if cell.occludes {
            bits |= OCCLUDES_BIT;
        }
        if cell.overlay {
            bits |= OVERLAY_BIT;
        }
        Self(bits)
    }

    /// The fluid-kind discriminator: `0` for no fluid, else [`KIND_WATER`] or
    /// [`KIND_LAVA`].
    #[inline]
    #[must_use]
    pub const fn kind_bits(self) -> u16 {
        self.0 & KIND_MASK
    }

    /// Whether this cell fully occludes an adjacent fluid face.
    #[inline]
    #[must_use]
    pub const fn occludes(self) -> bool {
        self.0 & OCCLUDES_BIT != 0
    }

    /// Whether a fluid side face touching this cell takes the overlay sprite.
    #[inline]
    #[must_use]
    pub const fn overlay(self) -> bool {
        self.0 & OVERLAY_BIT != 0
    }

    /// The fluid's own surface height, via [`FluidState::own_height`] so the
    /// float is produced by exactly the expression the un-gridded path used.
    ///
    /// Meaningless (and `0.0`-ish) for a cell with no fluid; callers check
    /// [`kind_bits`](Self::kind_bits) first, exactly as the old code checked
    /// `Some(f) if f.kind == kind`.
    #[inline]
    #[must_use]
    pub fn own_height(self) -> f32 {
        self.state().own_height()
    }

    /// The unpacked [`FluidState`].
    #[inline]
    #[must_use]
    pub const fn state(self) -> FluidState {
        FluidState {
            amount: ((self.0 >> AMOUNT_SHIFT) & AMOUNT_MASK) as u8,
            falling: self.0 & FALLING_BIT != 0,
        }
    }

    /// The whole cell back as a [`FluidCell`], or `None` for no fluid.
    #[inline]
    #[must_use]
    pub fn fluid(self) -> Option<FluidCell> {
        let kind = match self.kind_bits() {
            KIND_WATER => FluidKind::Water,
            KIND_LAVA => FluidKind::Lava,
            _ => return None,
        };
        Some(FluidCell {
            kind,
            state: self.state(),
        })
    }
}

/// The padded neighbourhood of one section, precomputed.
///
/// 11,664 bytes (`18³ × 2`), so it sits in L1 for the whole mesh of a section.
#[derive(Debug)]
pub struct FluidGrid {
    cells: Box<[PackedCell; GRID_CELLS]>,
    any_fluid: bool,
}

impl FluidGrid {
    /// Resolves the whole padded neighbourhood out of `view`.
    ///
    /// One [`FluidSectionView::cell_at`] call per interior cell, then one per
    /// shell cell within the fluid bounding box grown by one. Returns early
    /// (with [`any_fluid`](Self::any_fluid) `false`) when the section carries
    /// no fluid at all.
    #[must_use]
    pub fn build<V: FluidSectionView + ?Sized>(view: &V) -> Self {
        // Boxed rather than a stack array: 11,664 bytes of `[PackedCell; _]`
        // returned by value is a move the optimiser is not obliged to elide,
        // and the mesher runs on rayon workers whose stacks are not the main
        // thread's.
        let mut cells = vec![PackedCell::EMPTY; GRID_CELLS];
        let n = SECTION_SIZE as i32;

        // Pass 1: the interior, plus the bounding box of the cells that carry
        // fluid. `lo > hi` on any axis afterwards means "no fluid".
        let mut lo = [n; 3];
        let mut hi = [-1i32; 3];
        for y in 0..n {
            for z in 0..n {
                for x in 0..n {
                    let packed = PackedCell::pack(view.cell_at(x, y, z));
                    cells[index(x, y, z)] = packed;
                    if packed.kind_bits() != KIND_NONE {
                        for (axis, v) in [x, y, z].into_iter().enumerate() {
                            lo[axis] = lo[axis].min(v);
                            hi[axis] = hi[axis].max(v);
                        }
                    }
                }
            }
        }
        let cells: Box<[PackedCell; GRID_CELLS]> = cells
            .into_boxed_slice()
            .try_into()
            .expect("the vec was allocated with exactly GRID_CELLS elements");
        if lo[0] > hi[0] {
            return Self {
                cells,
                any_fluid: false,
            };
        }

        let mut grid = Self {
            cells,
            any_fluid: true,
        };
        // Pass 2: the shell, restricted to the fluid bounding box grown by one
        // — the exact set `mesh_fluids` can reach, since every probe is at most
        // one cell away in each axis. Clamped to the padded range, which the
        // grow can only just touch (`0 - 1 == -PAD`, `15 + 1 == n`).
        for y in (lo[1] - PAD).max(-PAD)..=(hi[1] + PAD).min(n) {
            for z in (lo[2] - PAD).max(-PAD)..=(hi[2] + PAD).min(n) {
                for x in (lo[0] - PAD).max(-PAD)..=(hi[0] + PAD).min(n) {
                    let interior = (0..n).contains(&x) && (0..n).contains(&y) && (0..n).contains(&z);
                    if !interior {
                        grid.cells[index(x, y, z)] = PackedCell::pack(view.cell_at(x, y, z));
                    }
                }
            }
        }
        grid
    }

    /// Whether any cell of the *section* (not the padding) carries a fluid.
    ///
    /// `false` means `mesh_fluids` has nothing to emit and can return
    /// immediately — the "contains no fluid" precheck, as a by-product of the
    /// fill rather than a separate palette scan.
    #[inline]
    #[must_use]
    pub const fn any_fluid(&self) -> bool {
        self.any_fluid
    }

    /// The packed cell at padded `(x, y, z)`, each in `-1..=16`.
    ///
    /// # Panics
    ///
    /// In debug builds, if a coordinate is outside the padded range — which
    /// would mean `mesh_fluids` grew a probe the grid does not cover, the one
    /// way this optimisation could silently change output.
    #[inline]
    #[must_use]
    pub fn get(&self, x: i32, y: i32, z: i32) -> PackedCell {
        let n = SECTION_SIZE as i32;
        debug_assert!(
            (-PAD..=n).contains(&x) && (-PAD..=n).contains(&y) && (-PAD..=n).contains(&z),
            "fluid grid probe ({x}, {y}, {z}) is outside the padded -{PAD}..={n} range"
        );
        self.cells[index(x, y, z)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_spans_exactly_the_reach_of_the_mesher() {
        // 18³, derived, not restated: the section plus one cell of padding.
        assert_eq!(GRID_DIM, 18);
        assert_eq!(GRID_CELLS, 5832);
        // 2 bytes per cell — 11,664, comfortably inside a 128 KiB L1.
        assert_eq!(size_of::<PackedCell>() * GRID_CELLS, 11_664);
        // The corners of the padded box must be distinct indices in range.
        let n = SECTION_SIZE as i32;
        let mut seen = std::collections::BTreeSet::new();
        for x in [-PAD, n] {
            for y in [-PAD, n] {
                for z in [-PAD, n] {
                    let i = index(x, y, z);
                    assert!(i < GRID_CELLS, "corner ({x},{y},{z}) indexes {i}");
                    assert!(seen.insert(i), "corner ({x},{y},{z}) aliases another");
                }
            }
        }
        assert_eq!(seen.len(), 8);
    }

    #[test]
    fn packing_round_trips_every_reachable_fluid_cell() {
        // Every `(kind, amount, falling, occludes, overlay)` a real cell can
        // carry must survive the pack — the one way the grid could change a
        // rendered height is by losing a bit of `amount`.
        for kind in [FluidKind::Water, FluidKind::Lava] {
            for amount in 1..=8u8 {
                for falling in [false, true] {
                    for occludes in [false, true] {
                        for overlay in [false, true] {
                            let cell = FluidNeighborCell {
                                fluid: Some(FluidCell {
                                    kind,
                                    state: FluidState { amount, falling },
                                }),
                                occludes,
                                overlay,
                            };
                            let p = PackedCell::pack(cell);
                            assert_eq!(p.fluid(), cell.fluid, "{cell:?}");
                            assert_eq!(p.occludes(), occludes, "{cell:?}");
                            assert_eq!(p.overlay(), overlay, "{cell:?}");
                            assert_eq!(
                                p.own_height(),
                                FluidState { amount, falling }.own_height(),
                                "{cell:?}"
                            );
                            assert_ne!(p.kind_bits(), KIND_NONE);
                        }
                    }
                }
            }
        }
        // And a fluid-free cell keeps its two booleans while reading as no
        // fluid, so `kind_bits() == centre_kind` can never match it.
        for occludes in [false, true] {
            for overlay in [false, true] {
                let p = PackedCell::pack(FluidNeighborCell {
                    fluid: None,
                    occludes,
                    overlay,
                });
                assert_eq!(p.fluid(), None);
                assert_eq!(p.kind_bits(), KIND_NONE);
                assert_eq!(p.occludes(), occludes);
                assert_eq!(p.overlay(), overlay);
            }
        }
    }

    /// A view with one water cell in a corner, so the shell fill has a small
    /// bounding box — and a counter proving the fill is bounded by it rather
    /// than covering the whole shell.
    struct Corner {
        probes: std::cell::Cell<usize>,
    }

    impl FluidSectionView for Corner {
        fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
            self.probes.set(self.probes.get() + 1);
            ((x, y, z) == (0, 0, 0)).then(|| FluidCell {
                kind: FluidKind::Water,
                state: FluidState::source(),
            })
        }

        fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
            false
        }

        fn fluid_sprites(&self, _kind: FluidKind) -> crate::block_models::FluidSprites {
            unreachable!("the grid never asks for sprites")
        }
    }

    #[test]
    fn the_shell_fill_is_bounded_by_the_fluid_bounding_box() {
        let view = Corner {
            probes: std::cell::Cell::new(0),
        };
        let grid = FluidGrid::build(&view);
        assert!(grid.any_fluid());
        assert_eq!(grid.get(0, 0, 0).kind_bits(), KIND_WATER);
        assert_eq!(grid.get(0, 0, 0).state(), FluidState::source());
        assert_eq!(grid.get(1, 0, 0).kind_bits(), KIND_NONE);

        // 4096 interior probes are unavoidable. The bounding box is the single
        // cell (0,0,0), grown to -1..=1 on each axis: 27 cells, of which 8 are
        // interior (0..=1 cubed), so 19 shell probes. A whole-shell fill would
        // be 18³ - 16³ = 1,736. The predicate is the *count*, not "fewer".
        let interior = 16 * 16 * 16;
        assert_eq!(
            view.probes.get(),
            interior + 19,
            "expected {interior} interior + 19 shell probes (bounded fill); a whole-shell \
             fill would be {interior} + 1736"
        );
    }

    #[test]
    fn a_fluid_free_section_stops_after_the_interior_pass() {
        struct Dry(std::cell::Cell<usize>);
        impl FluidSectionView for Dry {
            fn fluid_at(&self, _x: i32, _y: i32, _z: i32) -> Option<FluidCell> {
                self.0.set(self.0.get() + 1);
                None
            }
            fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
                false
            }
            fn fluid_sprites(&self, _kind: FluidKind) -> crate::block_models::FluidSprites {
                unreachable!()
            }
        }
        let view = Dry(std::cell::Cell::new(0));
        let grid = FluidGrid::build(&view);
        assert!(!grid.any_fluid());
        assert_eq!(view.0.get(), 16 * 16 * 16, "no shell probe for a dry section");
    }
}
