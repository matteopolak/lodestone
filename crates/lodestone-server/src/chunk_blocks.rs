//! Bit-packed, per-section block-index storage for [`crate::chunk::ChunkColumn`].
//!
//! # What it is
//!
//! [`SectionedBlocks`] replaces the flat `Vec<u16>` a `ChunkColumn` used to hold
//! over its full height. It is the *same* logical grid — palette indices in
//! `(y_local * 16 + z) * 16 + x` order, indexing the column's own block-state
//! palette — stored as one independent 16-row section at a time, each either a
//! single repeated id or a packed array whose width is sized to the ids that
//! section actually references.
//!
//! # Why it exists: this is where the render-distance RSS went
//!
//! `crate::chunk_store`'s module docs record a measured **195.5 KiB per retained
//! column**, of which `16 × 16 × 384 × 2 B = 192 KiB` was the flat grid. At
//! `render_distance` 32 that is 4,539 retained columns ≈ **867 MiB** — paid
//! identically by a column of solid stone and a column of pure air. That was unit
//! **U8** of `docs/plans/chunk-lifecycle.md`, deliberately gated on a measurement
//! rather than on arithmetic; the measurement arrived and this is the fix.
//!
//! Two independent savings, and the first is much the larger:
//!
//! * **An all-one-value section allocates nothing at all** ([`Section::Uniform`]).
//!   A full overworld column is 24 sections and terrain occupies roughly the lower
//!   half, so on the order of half of every column was 4,096 cells of `0`.
//!   Vanilla's own chunk format has exactly this case and stores no `data` array
//!   for it; so does the *client* already
//!   ([`lodestone_world`'s `Storage::Single`]).
//! * **A populated section packs to the width its ids need**, not to 16 bits. A
//!   deep section referencing palette ids `0..16` is 4 bits — a 4× cut — and an
//!   air/stone section is 1 bit.
//!
//! # How it works
//!
//! One [`Section`] per 16-row window counted from `min_y`, the same windows
//! `crate::chunk::SECTION_ROWS` governs and `ChunkColumn::section_ticking`
//! indexes. A section is either:
//!
//! * `Uniform(id)` — every cell holds `id`. Zero heap bytes.
//! * `Packed { bits, words }` — ids at `bits` wide, `64 / bits` values per `u64`
//!   with **no value spanning a long boundary** (the same non-spanning layout
//!   `lodestone_world::PackedArray` uses). `bits` is always wide enough for the
//!   largest id the section currently holds.
//!
//! `bits` only ever grows, and only on a [`set`](SectionedBlocks::set) that writes
//! an id the current width cannot hold; the widening rebuilds that one section
//! (up to 4,096 reads). Immutable generation handles share the section spine;
//! a resident write detaches shared storage before changing it. A section never narrows and never collapses
//! back to `Uniform` — both would be pure bookkeeping for a case that does not
//! recur, since a column is built once and then edited a handful of times.
//!
//! # Why no per-section palette, unlike the client's container
//!
//! `lodestone_world::PalettedContainer` keeps a *local* palette per section, so a
//! section holding one high-id block among stone stays at 4 bits where this stays
//! at whatever that one id needs. That is a real difference and it was measured as
//! not worth having here: the column palette is already deduplicated across the
//! whole column (tens of entries, so ≤ 7 bits), the remaining gap is single-KiB
//! per section, and a local palette costs a remap table plus an index rewrite on
//! every palette growth — the one operation in this file that must not have a bug,
//! because a wrong remap silently serves the wrong block rather than failing.
//! The client-facing container uses a `u32` index domain and an independent
//! local palette. Server generation snapshots instead share the same `u16`
//! column-index domain as the compact generated product.
//!
//! # How to change it
//!
//! The one invariant: **`get` after `set` must return what was written, for every
//! cell, at every width**. The gates at the bottom of this file drive the width
//! transitions explicitly (`1 → 4 → 8 → 9 → 16`) because that is where a packing
//! bug lives, and `crate::chunk`'s own byte-identity gate compares a whole real
//! generated column cell-by-cell against the flat representation.
//!
//! Storage and width changes belong in
//! `lodestone_worldgen::generated_storage::CompactBlockStorage`, which also
//! owns partial-section bounds and snapshot copy-on-write.
//!
//! # Configuration
//!
//! None. Sections have 16 rows and 4,096 cells; widths are derived.
//!
//! # Dependencies
//!
//! `lodestone-worldgen` supplies the shared compact section implementation.
//!
//! [`lodestone_world`'s `Storage::Single`]: https://docs.rs/lodestone-world

#[cfg(test)]
use crate::chunk::SECTION_ROWS;
use lodestone_worldgen::generated_storage::{CompactBlockStorage, CompactSection as Section};

type Id = u16;
#[cfg(test)]
const CELLS: usize = SECTION_ROWS * 16 * 16;

#[cfg(test)]
fn bits_for_id(max_id: Id) -> u32 {
    (Id::BITS - max_id.leading_zeros()).max(1)
}

/// Server-owned palette indices with shared immutable section snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionedBlocks {
    storage: CompactBlockStorage,
}

impl SectionedBlocks {
    pub(crate) fn new_air(height: i32) -> Self {
        Self { storage: CompactBlockStorage::uniform(0, height, 0) }
    }

    #[cfg(test)]
    pub(crate) fn from_flat(height: i32, cells: &[Id]) -> Self {
        let (storage, _) = CompactBlockStorage::from_section_fn_with_predicates(
            0, height, 0, false, None, None, [Id::MAX; 2],
            |section, target| {
                let start = section * CELLS;
                target.copy_from_slice(&cells[start..start + target.len()]);
                None
            },
        );
        Self { storage }
    }

    #[cfg(test)]
    pub(crate) fn from_flat_with_observer(
        height: i32,
        cells: &[Id],
        mut observer: impl FnMut(usize, Id),
    ) -> Self {
        for (index, &id) in cells.iter().enumerate().rev() {
            observer(index, id);
        }
        Self::from_flat(height, cells)
    }

    pub(crate) fn from_compact(storage: CompactBlockStorage) -> Self {
        Self { storage }
    }

    pub(crate) fn section_count(&self) -> usize {
        self.storage.section_count()
    }

    /// First section above every nonzero index. Zero must denote ordinary air;
    /// other air variants and unused partial rows can only overestimate this bound.
    pub(crate) fn air_ceiling_section(&self) -> usize {
        self.storage.sections().iter().rposition(|section| match section {
            Section::Uniform(id) => *id != 0,
            Section::Packed { words, .. } => words.iter().any(|&word| word != 0),
        }).map_or(0, |index| index + 1)
    }

    #[inline]
    pub(crate) fn uniform_id(&self, section: usize) -> Option<Id> {
        self.storage.section(section)?.uniform_id()
    }

    #[cfg(test)]
    fn section_rows(&self, section: usize) -> usize {
        self.storage.section_rows(section)
    }

    #[inline]
    pub(crate) fn get(&self, x: i32, y_local: i32, z: i32) -> Id {
        self.storage.get(x as usize, self.storage.min_y() + y_local, z as usize)
    }

    #[inline]
    pub(crate) fn set(&mut self, x: i32, y_local: i32, z: i32, id: Id) {
        self.storage.set(x as usize, self.storage.min_y() + y_local, z as usize, id);
    }

    pub(crate) fn for_each_in_section(&self, section: usize, f: impl FnMut(usize, Id)) {
        self.storage.for_each_section(section, f);
    }

    pub(crate) fn append_section_cells(&self, section: usize, out: &mut Vec<Id>) {
        self.storage.append_section_cells(section, out);
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.storage.heap_bytes()
    }

    #[cfg(test)]
    pub(crate) fn uniform_sections(&self) -> usize {
        self.storage.sections().iter().filter(|section| section.uniform_id().is_some()).count()
    }

    #[cfg(test)]
    pub(crate) fn section_bits(&self, section: usize) -> u32 {
        u32::from(self.storage.section(section).expect("section exists").bits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference implementation: the flat `Vec<u16>` this module replaced. Every
    /// gate below compares against *this*, not against `SectionedBlocks`'s own
    /// earlier output — `decode(encode(x)) == x` would be satisfied by two
    /// symmetric packing bugs.
    struct Flat {
        cells: Vec<Id>,
    }

    impl Flat {
        fn new(height: i32) -> Self {
            Self {
                cells: vec![0; 16 * 16 * height as usize],
            }
        }
        fn index(x: i32, y_local: i32, z: i32) -> usize {
            ((y_local * 16 + z) * 16 + x) as usize
        }
        fn set(&mut self, x: i32, y_local: i32, z: i32, id: Id) {
            self.cells[Self::index(x, y_local, z)] = id;
        }
        fn get(&self, x: i32, y_local: i32, z: i32) -> Id {
            self.cells[Self::index(x, y_local, z)]
        }
    }

    /// Deterministic pseudo-random ids, so a failure is reproducible. Not
    /// `rand`: this crate has no need of the dependency and a fixed sequence is
    /// strictly better evidence than a seeded one nobody records.
    fn scramble(n: usize) -> u64 {
        let mut h = n as u64 ^ 0x9E37_79B9_7F4A_7C15;
        h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        h ^ (h >> 31)
    }

    #[test]
    fn bits_for_id_is_exact_ceil_log2() {
        // Computed from the definition, not from the function: 1 is the floor,
        // and each power of two is the first id needing one more bit.
        for (id, bits) in [
            (0u16, 1u32),
            (1, 1),
            (2, 2),
            (3, 2),
            (4, 3),
            (15, 4),
            (16, 5),
            (255, 8),
            (256, 9),
            (u16::MAX, 16),
        ] {
            assert_eq!(bits_for_id(id), bits, "id {id}");
        }
    }

    #[test]
    fn an_all_air_column_allocates_no_cell_storage() {
        // The larger of the two savings, asserted as an exact byte count rather
        // than as "less than before". 24 sections, 0 packed bytes, spine only.
        let blocks = SectionedBlocks::new_air(384);
        assert_eq!(blocks.section_count(), 24);
        assert_eq!(blocks.uniform_sections(), 24);
        let spine = 24 * core::mem::size_of::<Section>();
        assert_eq!(
            blocks.heap_bytes(),
            spine,
            "an all-air column must own the section spine and nothing else; the flat \
             representation owned 16*16*384*2 = {} bytes",
            16 * 16 * 384 * 2
        );
        for y in 0..384 {
            assert_eq!(blocks.get(3, y, 9), 0, "row {y}");
        }
    }

    #[test]
    fn air_ceiling_tracks_the_highest_occupied_section() {
        let mut blocks = SectionedBlocks::new_air(48);
        assert_eq!(blocks.air_ceiling_section(), 0);
        blocks.set(2, 4, 3, 1);
        assert_eq!(blocks.air_ceiling_section(), 1);
        blocks.set(2, 40, 3, 2);
        assert_eq!(blocks.air_ceiling_section(), 3);
        blocks.set(2, 40, 3, 0);
        assert_eq!(blocks.air_ceiling_section(), 1);
    }

    #[test]
    fn every_write_reads_back_across_all_width_transitions() {
        // Drives 1 -> 2 -> 4 -> 8 -> 9 -> 16 bits inside one section by writing
        // ids that each force the next width, checking the WHOLE section against
        // the flat reference after each transition. A packing bug that corrupts
        // neighbours rather than the written cell is exactly what this catches.
        let mut flat = Flat::new(16);
        let mut packed = SectionedBlocks::new_air(16);
        for (step, &id) in [1u16, 2, 8, 15, 16, 200, 256, 4000, u16::MAX]
            .iter()
            .enumerate()
        {
            // Spread the writes so successive ids land in different longs.
            let y = (step * 3 % 16) as i32;
            let z = (step * 5 % 16) as i32;
            let x = (step * 7 % 16) as i32;
            flat.set(x, y, z, id);
            packed.set(x, y, z, id);
            assert_eq!(
                packed.section_bits(0),
                bits_for_id(*flat.cells.iter().max().unwrap()),
                "step {step}: width must track the largest id present"
            );
            for yy in 0..16 {
                for zz in 0..16 {
                    for xx in 0..16 {
                        assert_eq!(
                            packed.get(xx, yy, zz),
                            flat.get(xx, yy, zz),
                            "step {step} (wrote {id}): cell ({xx}, {yy}, {zz})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_scrambled_full_height_column_round_trips_cell_for_cell() {
        // Full overworld height, every cell written to a pseudo-random id, then
        // every cell read back. This is the whole-representation identity check:
        // 98,304 cells, no sampling.
        let mut flat = Flat::new(384);
        let mut packed = SectionedBlocks::new_air(384);
        let mut n = 0usize;
        for y in 0..384 {
            for z in 0..16 {
                for x in 0..16 {
                    // Bias toward small ids (a real palette is tens of entries)
                    // but include a few large ones so some sections widen.
                    let id = if n % 997 == 0 {
                        (scramble(n) % 60_000) as Id
                    } else {
                        (scramble(n) % 40) as Id
                    };
                    flat.set(x, y, z, id);
                    packed.set(x, y, z, id);
                    n += 1;
                }
            }
        }
        for y in 0..384 {
            for z in 0..16 {
                for x in 0..16 {
                    assert_eq!(
                        packed.get(x, y, z),
                        flat.get(x, y, z),
                        "cell ({x}, {y}, {z})"
                    );
                }
            }
        }

        // `from_flat` must land on the identical content as the incremental
        // writes above — the two construction paths production uses.
        let adopted = SectionedBlocks::from_flat(384, &flat.cells);
        for y in 0..384 {
            for z in 0..16 {
                for x in 0..16 {
                    assert_eq!(
                        adopted.get(x, y, z),
                        flat.get(x, y, z),
                        "from_flat cell ({x}, {y}, {z})"
                    );
                }
            }
        }
    }

    #[test]
    fn from_flat_collapses_air_and_sizes_the_rest_to_its_ids() {
        // A terrain-shaped column: stone/dirt below y=0, air above. Predict both
        // the uniform count and the exact widths, so the assertion fails under
        // either a missed collapse or an over-wide packing.
        let height = 384;
        let mut cells = vec![0u16; 16 * 16 * height as usize];
        for y_local in 0..64 {
            for z in 0..16 {
                for x in 0..16 {
                    // ids 1 and 2 only => 2 bits.
                    cells[((y_local * 16 + z) * 16 + x) as usize] = if y_local % 3 == 0 { 1 } else { 2 };
                }
            }
        }
        let blocks = SectionedBlocks::from_flat(height, &cells);
        assert_eq!(blocks.section_count(), 24);
        assert_eq!(
            blocks.uniform_sections(),
            20,
            "sections 4..24 are pure air and section 0..4 are populated"
        );
        for s in 0..4 {
            assert_eq!(blocks.section_bits(s), 2, "section {s}: ids 1 and 2 need 2 bits");
        }
        // 4 sections x 4096 cells x 2 bits = 4 x 1024 bytes, plus the spine.
        let spine = 24 * core::mem::size_of::<Section>();
        assert_eq!(
            blocks.heap_bytes(),
            4 * 1024 + spine,
            "predicted exactly; the flat grid was {} bytes",
            16 * 16 * height as usize * 2
        );
    }

    #[test]
    fn observed_packing_is_byte_identical_for_uniform_low_and_high_sections() {
        for (ids, expected_bits, expected_uniform) in [
            (vec![0u16; CELLS], 0, true),
            ((0..CELLS).map(|i| (i as u16 & 1) + 1).collect(), 2, false),
            ((0..CELLS).map(|i| 0x8000 | (i as u16 & 1)).collect(), 16, false),
        ] {
            let plain = SectionedBlocks::from_flat(16, &ids);
            let mut observed_cells = 0;
            let observed = SectionedBlocks::from_flat_with_observer(16, &ids, |index, id| {
                assert_eq!(ids[index], id);
                observed_cells += 1;
            });
            assert_eq!(observed, plain);
            assert_eq!(observed_cells, CELLS);
            assert_eq!(observed.uniform_sections() == 1, expected_uniform);
            assert_eq!(observed.section_bits(0), expected_bits);
        }
    }

    #[test]
    fn append_section_cells_reproduces_the_flat_slice_exactly() {
        // `chunk_nbt` slices the old flat grid per section; this is the same
        // bytes, and the order is load-bearing for the region file.
        let height = 48;
        let mut cells = vec![0u16; 16 * 16 * height as usize];
        for (i, cell) in cells.iter_mut().enumerate() {
            *cell = (scramble(i) % 300) as Id;
        }
        let blocks = SectionedBlocks::from_flat(height, &cells);
        for s in 0..blocks.section_count() {
            let mut out = Vec::new();
            blocks.append_section_cells(s, &mut out);
            let base = s * CELLS;
            assert_eq!(
                out.as_slice(),
                &cells[base..base + CELLS],
                "section {s} cells must match the flat slice"
            );
        }
    }

    #[test]
    fn a_partial_top_section_reports_only_its_real_rows() {
        // Height 20 => sections of 16 and 4 rows. The old flat grid held
        // 16*16*20 cells; the surplus 12 rows of section 1 must never be
        // reported, or `chunk_nbt` would write cells that did not exist.
        let blocks = SectionedBlocks::new_air(20);
        assert_eq!(blocks.section_count(), 2);
        assert_eq!(blocks.section_rows(0), 16);
        assert_eq!(blocks.section_rows(1), 4);
        let mut out = Vec::new();
        blocks.append_section_cells(1, &mut out);
        assert_eq!(out.len(), 4 * 16 * 16);
    }

    #[test]
    fn a_uniform_section_survives_a_same_value_write_without_allocating() {
        // The no-op path: rewriting the value already present must not promote
        // to `Packed`, or every `set_block` of air onto air would allocate.
        let mut blocks = SectionedBlocks::new_air(16);
        blocks.set(1, 1, 1, 0);
        assert_eq!(blocks.uniform_sections(), 1);
        assert_eq!(blocks.heap_bytes(), core::mem::size_of::<Section>());
    }

    #[test]
    fn promoting_a_non_zero_uniform_preserves_every_other_cell() {
        // The seeding branch: a section uniformly full of id 7 that takes one
        // write of id 9 must keep 4,095 cells at 7. Zero-filling the new buffer
        // and forgetting to seed it would leave them all air — a silent terrain
        // deletion, and the one bug in this file with no loud symptom.
        let mut blocks = SectionedBlocks::from_flat(16, &vec![7u16; CELLS]);
        assert_eq!(blocks.uniform_sections(), 1);
        blocks.set(5, 5, 5, 9);
        assert_eq!(blocks.section_bits(0), 4, "ids up to 9 need 4 bits");
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let expected = if (x, y, z) == (5, 5, 5) { 9 } else { 7 };
                    assert_eq!(blocks.get(x, y, z), expected, "cell ({x}, {y}, {z})");
                }
            }
        }
    }
}
