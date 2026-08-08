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
//! * `Packed { bits, longs }` — ids at `bits` wide, `64 / bits` values per `u64`
//!   with **no value spanning a long boundary** (the same non-spanning layout
//!   `lodestone_world::PackedArray` uses). `bits` is always wide enough for the
//!   largest id the section currently holds.
//!
//! `bits` only ever grows, and only on a [`set`](SectionedBlocks::set) that writes
//! an id the current width cannot hold; the widening rebuilds that one section
//! (4,096 reads) and nothing else. A section never narrows and never collapses
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
//! Reusing `PalettedContainer` outright was also considered and rejected: it is a
//! `u32` container that would need a 4,096-entry `Vec<u32>` marshalling buffer at
//! every construction and repeats its 32-byte `PaletteKind` in all 24 sections,
//! and it lives in a crate that `lodestone-server` deliberately keeps out of its
//! normal dependency graph (see this crate's `Cargo.toml`, where `lodestone-world`
//! is dev-only, and `src/ecs/schedules.rs` for why the browser bundle cares).
//!
//! # How to change it
//!
//! The one invariant: **`get` after `set` must return what was written, for every
//! cell, at every width**. The gates at the bottom of this file drive the width
//! transitions explicitly (`1 → 4 → 8 → 9 → 16`) because that is where a packing
//! bug lives, and `crate::chunk`'s own byte-identity gate compares a whole real
//! generated column cell-by-cell against the flat representation.
//!
//! If you widen `Id` past `u16`, note that `bits_for_id` derives the width from
//! `Id::BITS` rather than a literal, and that `MAX_BITS` asserts a value still
//! fits one `u64`.
//!
//! # Configuration
//!
//! None. No constant here is a tuning knob: [`CELLS`] and [`SECTION_ROWS`] are the
//! chunk format, and the widths are derived.
//!
//! # Dependencies
//!
//! None outside `core`. Deliberately: this is the hottest data structure in the
//! server and the crate's dependency graph is load-bearing for the browser bundle.
//!
//! [`lodestone_world`'s `Storage::Single`]: https://docs.rs/lodestone-world

use crate::chunk::SECTION_ROWS;

/// A palette index. The column's palette is `Vec<String>`, so this is an index
/// into it and never a registry id.
type Id = u16;

/// Cells in one section: `16 × 16 × 16`.
pub(crate) const CELLS: usize = SECTION_ROWS * 16 * 16;

/// Cells in one Y row of a section (`16 × 16`) — the stride
/// `(y_local * 16 + z) * 16 + x` advances per row.
const ROW_CELLS: usize = 16 * 16;

/// Widest packing this file will produce. `Id` is `u16`, and 16 divides 64, so
/// the worst case is still a whole number of values per long.
const MAX_BITS: u32 = Id::BITS;

const _: () = assert!(MAX_BITS <= 64, "a value must fit inside one u64");

/// Bits needed to represent every id in `0..=max_id`, floored at 1.
///
/// `ceil(log2(max_id + 1))` via the highest set bit, so it is exact rather than a
/// float approximation. Floored at 1 (not 4, and not 0) because a section holding
/// only air is [`Section::Uniform`] and never reaches here, so the narrowest real
/// packed section is the air/stone pair — genuinely 1 bit, and there is no wire
/// format imposing vanilla's 4-bit minimum on this purely internal layout.
fn bits_for_id(max_id: Id) -> u32 {
    (Id::BITS - max_id.leading_zeros()).max(1)
}

/// Values packed into one `u64` at `bits` wide, with none spanning a boundary.
#[inline]
fn values_per_long(bits: u32) -> usize {
    (64 / bits) as usize
}

/// `u64`s needed to hold [`CELLS`] values at `bits` wide.
#[inline]
fn long_count(bits: u32) -> usize {
    CELLS.div_ceil(values_per_long(bits))
}

/// One 16-row window's worth of palette indices.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Section {
    /// Every cell holds this id. **Allocates nothing** — the whole reason this
    /// module exists, since most sections of most columns are pure air.
    Uniform(Id),
    /// Ids packed at `bits` wide. `bits >= bits_for_id(largest id present)`.
    Packed { bits: u32, longs: Vec<u64> },
}

impl Section {
    #[inline]
    fn get(&self, index: usize) -> Id {
        debug_assert!(index < CELLS);
        match self {
            Self::Uniform(id) => *id,
            Self::Packed { bits, longs } => {
                let per = values_per_long(*bits);
                let slot = longs[index / per];
                let shift = (index % per) as u32 * *bits;
                let mask = (1u64 << *bits) - 1;
                ((slot >> shift) & mask) as Id
            }
        }
    }

    /// Writes `id`, widening or promoting out of [`Uniform`](Self::Uniform) if
    /// the current representation cannot hold it.
    fn set(&mut self, index: usize, id: Id) {
        debug_assert!(index < CELLS);
        match self {
            Self::Uniform(current) => {
                if *current == id {
                    return;
                }
                let current = *current;
                let bits = bits_for_id(current.max(id));
                let mut longs = vec![0u64; long_count(bits)];
                if current != 0 {
                    // Only the non-zero uniform needs seeding; a zeroed buffer
                    // already reads back as id 0 everywhere.
                    let per = values_per_long(bits);
                    let mut word = 0u64;
                    for slot in 0..per {
                        word |= u64::from(current) << (slot as u32 * bits);
                    }
                    longs.fill(word);
                }
                *self = Self::Packed { bits, longs };
                self.set(index, id);
            }
            Self::Packed { bits, longs } => {
                let needed = bits_for_id(id);
                if needed > *bits {
                    let wider = needed;
                    let mut next = vec![0u64; long_count(wider)];
                    let per_old = values_per_long(*bits);
                    let per_new = values_per_long(wider);
                    let mask_old = (1u64 << *bits) - 1;
                    for cell in 0..CELLS {
                        let value =
                            (longs[cell / per_old] >> ((cell % per_old) as u32 * *bits)) & mask_old;
                        next[cell / per_new] |= value << ((cell % per_new) as u32 * wider);
                    }
                    *bits = wider;
                    *longs = next;
                }
                let per = values_per_long(*bits);
                let shift = (index % per) as u32 * *bits;
                let mask = (1u64 << *bits) - 1;
                let slot = &mut longs[index / per];
                *slot = (*slot & !(mask << shift)) | (u64::from(id) << shift);
            }
        }
    }

    /// Heap bytes this section owns. `0` for [`Uniform`](Self::Uniform), which is
    /// the measurement the whole change rests on.
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Uniform(_) => 0,
            Self::Packed { longs, .. } => longs.capacity() * core::mem::size_of::<u64>(),
        }
    }
}

/// A column's block-index grid, stored one 16-row section at a time. See the
/// module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionedBlocks {
    sections: Vec<Section>,
    /// Block rows this grid covers, from the column's `min_y` upward. Sections
    /// always hold a full [`CELLS`], so a `height` that is not a multiple of
    /// [`SECTION_ROWS`] leaves the top section's surplus rows addressable but
    /// never *reported* — see [`section_rows`](Self::section_rows), which is what
    /// every bulk reader sizes itself by.
    height: i32,
}

impl SectionedBlocks {
    /// An all-air grid (`id 0` everywhere) of `height` rows. Allocates one
    /// [`Section::Uniform`] per section and **no cell storage at all**.
    pub(crate) fn new_air(height: i32) -> Self {
        debug_assert!(height > 0);
        let sections = (height as usize).div_ceil(SECTION_ROWS);
        Self {
            sections: vec![Section::Uniform(0); sections],
            height,
        }
    }

    /// Adopts a flat `(y_local * 16 + z) * 16 + x` grid — the layout
    /// [`lodestone_worldgen::overworld::GeneratedColumn`] hands over and the one
    /// `ChunkColumn` used to store directly.
    ///
    /// Each section independently picks the narrowest representation for its own
    /// content, so this is where an air section above the terrain surface becomes
    /// free.
    pub(crate) fn from_flat(height: i32, cells: &[Id]) -> Self {
        debug_assert!(height > 0);
        debug_assert_eq!(cells.len(), 16 * 16 * height as usize);
        let section_count = (height as usize).div_ceil(SECTION_ROWS);
        let mut sections = Vec::with_capacity(section_count);
        for s in 0..section_count {
            let base = s * CELLS;
            let slice = &cells[base..(base + CELLS).min(cells.len())];
            sections.push(Self::pack(slice));
        }
        Self { sections, height }
    }

    /// Chooses the narrowest [`Section`] for `slice`, which may be shorter than
    /// [`CELLS`] for a partial top section (its surplus cells read back as 0).
    fn pack(slice: &[Id]) -> Section {
        let first = slice.first().copied().unwrap_or(0);
        // A partial top section collapses to `Uniform(first)` too, even though
        // that makes its surplus cells read back as `first` rather than as the
        // flat grid's implicit 0. Sound because no reader can reach them:
        // `section_rows` bounds every bulk read and `get` is only ever called
        // with a `y_local` inside `height`.
        if slice.iter().all(|&id| id == first) {
            return Section::Uniform(first);
        }
        let max_id = slice.iter().copied().max().unwrap_or(0);
        let bits = bits_for_id(max_id);
        let per = values_per_long(bits);
        let mut longs = vec![0u64; long_count(bits)];
        for (cell, &id) in slice.iter().enumerate() {
            longs[cell / per] |= u64::from(id) << ((cell % per) as u32 * bits);
        }
        Section::Packed { bits, longs }
    }

    /// Sections in this grid — `height / 16`, rounded up.
    pub(crate) fn section_count(&self) -> usize {
        self.sections.len()
    }

    /// Real block rows in section `s`. `16` for every section but a partial top
    /// one, and `0` past the end.
    ///
    /// Bulk readers size themselves by this rather than by [`CELLS`] so that a
    /// column whose height is not a multiple of 16 reports exactly the cells the
    /// old flat `Vec<u16>` held, not the section's zero-padded surplus.
    pub(crate) fn section_rows(&self, s: usize) -> usize {
        let start = s * SECTION_ROWS;
        (self.height as usize).saturating_sub(start).min(SECTION_ROWS)
    }

    /// Flat cell index within a section for local `(x, z)` and a `y_local` that
    /// may be anywhere in the column.
    #[inline]
    fn cell_index(x: i32, y_local: i32, z: i32) -> usize {
        debug_assert!((0..16).contains(&x));
        debug_assert!((0..16).contains(&z));
        let row = (y_local as usize % SECTION_ROWS) * ROW_CELLS;
        row + (z as usize) * 16 + (x as usize)
    }

    /// The palette index at local `(x, z)` and `y_local` (rows from the column's
    /// `min_y`).
    #[inline]
    pub(crate) fn get(&self, x: i32, y_local: i32, z: i32) -> Id {
        debug_assert!((0..self.height).contains(&y_local));
        self.sections[y_local as usize / SECTION_ROWS].get(Self::cell_index(x, y_local, z))
    }

    /// Writes the palette index at local `(x, z)` and `y_local`.
    #[inline]
    pub(crate) fn set(&mut self, x: i32, y_local: i32, z: i32, id: Id) {
        debug_assert!((0..self.height).contains(&y_local));
        let index = Self::cell_index(x, y_local, z);
        self.sections[y_local as usize / SECTION_ROWS].set(index, id);
    }

    /// Calls `f(cell_index_within_section, id)` for every **real** cell of
    /// section `s`, in flat order.
    ///
    /// This is the bulk-read primitive: `crate::chunk_nbt`'s per-section palette
    /// remap, `ChunkColumn::recalc_ticking_counts` and `solid_count` all go
    /// through it, so none of them materialises a whole column. A `Uniform`
    /// section costs one `match` and no memory traffic.
    pub(crate) fn for_each_in_section(&self, s: usize, mut f: impl FnMut(usize, Id)) {
        let rows = self.section_rows(s);
        if rows == 0 {
            return;
        }
        let section = &self.sections[s];
        match section {
            // Hoisted out of the loop: the whole point of `Uniform` is that it
            // reads without touching memory, and the per-cell `match` would
            // otherwise reintroduce a branch 4,096 times.
            Section::Uniform(id) => {
                for cell in 0..rows * ROW_CELLS {
                    f(cell, *id);
                }
            }
            Section::Packed { .. } => {
                for cell in 0..rows * ROW_CELLS {
                    f(cell, section.get(cell));
                }
            }
        }
    }

    /// Appends section `s`'s real cells to `out`, in flat
    /// `(y_in_section * 16 + z) * 16 + x` order — vanilla's own section order, so
    /// a caller can slice it straight into a chunk-format container.
    pub(crate) fn append_section_cells(&self, s: usize, out: &mut Vec<Id>) {
        out.reserve(self.section_rows(s) * ROW_CELLS);
        self.for_each_in_section(s, |_, id| out.push(id));
    }

    /// Heap bytes the packed arrays own, excluding the `Vec<Section>` spine.
    ///
    /// Exists for the residency gate in `crate::chunk` — a *count* of bytes rather
    /// than an RSS reading, so it is immune to machine load and can assert the
    /// representation's cost directly.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.sections.iter().map(Section::heap_bytes).sum::<usize>()
            + self.sections.capacity() * core::mem::size_of::<Section>()
    }

    /// How many sections allocate no cell storage at all. The direct measurement
    /// of the larger of this module's two savings.
    #[cfg(test)]
    pub(crate) fn uniform_sections(&self) -> usize {
        self.sections
            .iter()
            .filter(|s| matches!(s, Section::Uniform(_)))
            .count()
    }

    /// Per-section packing width, `0` for a uniform section. Test-only: the gates
    /// below predict exact widths rather than asserting "smaller".
    #[cfg(test)]
    pub(crate) fn section_bits(&self, s: usize) -> u32 {
        match &self.sections[s] {
            Section::Uniform(_) => 0,
            Section::Packed { bits, .. } => *bits,
        }
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
        assert_eq!(out.len(), 4 * ROW_CELLS);
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
