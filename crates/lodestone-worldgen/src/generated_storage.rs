//! Section-aligned block-index storage for generated columns.
//!
//! [`CompactBlockStorage`] keeps one column-wide palette index space while
//! representing each 16-row section as either one repeated value or a packed
//! `u16` index stream. The representation is deliberately independent of block
//! names and registries: the palette belongs to the generated column, and this
//! type only owns its indices.

const SECTION_ROWS: usize = 16;
const ROW_CELLS: usize = 16 * 16;
const SECTION_CELLS: usize = SECTION_ROWS * ROW_CELLS;
const VALUES_PER_WORD: [usize; 17] = [
    0, 64, 32, 21, 16, 12, 10, 9, 8, 7, 6, 5, 5, 4, 4, 4, 4,
];

/// A palette-index section of a generated column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactSection {
    /// Every real cell in this section has the same palette index.
    Uniform(u16),
    /// Palette indices packed least-significant first, without crossing word
    /// boundaries. `bits` is the width of each value.
    Packed { bits: u8, words: Vec<u64> },
}

impl CompactSection {
    #[inline]
    fn get(&self, cell: usize) -> u16 {
        match self {
            Self::Uniform(id) => *id,
            Self::Packed { bits, words } => {
                let bits = u32::from(*bits);
                let per_word = values_per_word(bits);
                let word = words[cell / per_word];
                let shift = (cell % per_word) as u32 * bits;
                ((word >> shift) & mask(bits)) as u16
            }
        }
    }

    fn pack(slice: &[u16], rows: usize) -> Self {
        Self::pack_observed(slice, rows, |_, _| {})
    }

    fn pack_observed(
        slice: &[u16],
        rows: usize,
        mut observe: impl FnMut(usize, u16),
    ) -> Self {
        let first = slice.first().copied().unwrap_or(0);
        // Discover both properties in one pass.  The old pair of `all` and
        // `max` walks did two complete reads before the packing walk, which
        // made every mixed section pay three passes over its 4,096 cells.
        let mut uniform = true;
        let mut max_id = first;
        for (cell, &id) in slice.iter().enumerate() {
            observe(cell, id);
            if cell == 0 {
                continue;
            }
            uniform &= id == first;
            max_id = max_id.max(id);
        }
        if uniform {
            return Self::Uniform(first);
        }

        let bits = bits_for_id(max_id);
        let words = vec![0u64; packed_word_count(rows * ROW_CELLS, bits)];
        let mut section = Self::Packed { bits: bits as u8, words };
        if let Self::Packed { bits, words } = &mut section {
            let bits = u32::from(*bits);
            let mut word_index = 0usize;
            let mut shift = 0u32;
            for &id in slice {
                words[word_index] |= u64::from(id) << shift;
                shift += bits;
                // Leave the unused tail bits in place when `bits` does not
                // divide 64; this is the same layout as `values_per_word`.
                if shift + bits > u64::BITS {
                    word_index += 1;
                    shift = 0;
                }
            }
        }
        section
    }

    /// Number of bytes owned by this section's packed payload.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        match self {
            Self::Uniform(_) => 0,
            Self::Packed { words, .. } => words.capacity() * std::mem::size_of::<u64>(),
        }
    }

    /// Returns the section's packing width, or `0` for a uniform section.
    #[must_use]
    pub const fn bits(&self) -> u8 {
        match self {
            Self::Uniform(_) => 0,
            Self::Packed { bits, .. } => *bits,
        }
    }

    /// Returns the repeated palette index for a uniform section.
    #[must_use]
    pub const fn uniform_id(&self) -> Option<u16> {
        match self {
            Self::Uniform(id) => Some(*id),
            Self::Packed { .. } => None,
        }
    }

    /// Consumes this section into its representation parts.
    #[must_use]
    pub fn into_parts(self) -> CompactSectionParts {
        match self {
            Self::Uniform(id) => CompactSectionParts::Uniform(id),
            Self::Packed { bits, words } => CompactSectionParts::Packed { bits, words },
        }
    }
}

/// Owned section representation parts for a consumer that has a matching
/// section enum and wants to move packed words without rebuilding them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactSectionParts {
    /// One repeated palette index.
    Uniform(u16),
    /// Packed words and their width.
    Packed { bits: u8, words: Vec<u64> },
}

/// A generated column's compact block-index field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactBlockStorage {
    min_y: i32,
    height: i32,
    sections: Vec<CompactSection>,
}

impl CompactBlockStorage {
    /// Builds section storage from the flat generated-column layout
    /// `((ly * 16 + z) * 16 + x)`. The input is borrowed so callers can compare
    /// the compact result against an independent flat control before dropping
    /// the old carrier.
    #[must_use]
    pub fn from_flat(min_y: i32, height: i32, cells: &[u16]) -> Self {
        Self::from_flat_inner(min_y, height, cells, None)
    }

    /// Builds section storage while observing every final cell in the same
    /// section traversal used for packing. The optional palette predicate is
    /// indexed by each cell's palette id and produces the two vertical
    /// summaries needed by the generated-column output boundary.
    #[must_use]
    pub fn from_flat_with_summaries(
        min_y: i32,
        height: i32,
        cells: &[u16],
        motion_blocking: Option<&[bool]>,
    ) -> (Self, GeneratedColumnSummaries) {
        let mut summaries = GeneratedColumnSummaries::new(motion_blocking.is_some());
        let storage = Self::from_flat_inner(
            min_y,
            height,
            cells,
            Some((&mut summaries, motion_blocking)),
        );
        (storage, summaries)
    }

    fn from_flat_inner(
        min_y: i32,
        height: i32,
        cells: &[u16],
        mut summaries: Option<(&mut GeneratedColumnSummaries, Option<&[bool]>)>,
    ) -> Self {
        assert!(height >= 0, "column height is negative");
        let expected = (height as usize)
            .checked_mul(ROW_CELLS)
            .expect("column cell count overflows usize");
        assert_eq!(cells.len(), expected, "flat column length does not match height");
        let section_count = (height as usize).div_ceil(SECTION_ROWS);
        let mut sections = Vec::with_capacity(section_count);
        for section in 0..section_count {
            let start = section * SECTION_CELLS;
            let rows = (height as usize - section * SECTION_ROWS).min(SECTION_ROWS);
            let end = start + rows * ROW_CELLS;
            let slice = &cells[start..end];
            if let Some((summary, motion_blocking)) = summaries.as_mut() {
                sections.push(CompactSection::pack_observed(slice, rows, |cell, id| {
                    summary.observe(section * SECTION_ROWS, cell, id, *motion_blocking);
                }));
            } else {
                sections.push(CompactSection::pack(slice, rows));
            }
        }
        crate::counters::bump_full_column_conversion(cells.len() as u64);
        Self { min_y, height, sections }
    }

    /// World Y origin of this storage.
    #[must_use]
    pub const fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of real block rows in this storage.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.height
    }

    /// Number of 16-row sections, rounded up for a partial top section.
    #[must_use]
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }

    /// Real rows in section `section`, or zero past the top.
    #[must_use]
    pub fn section_rows(&self, section: usize) -> usize {
        let start = section.saturating_mul(SECTION_ROWS);
        (self.height as usize).saturating_sub(start).min(SECTION_ROWS)
    }

    /// Borrowed section view for a zero-copy adapter.
    #[must_use]
    pub fn section(&self, section: usize) -> Option<&CompactSection> {
        self.sections.get(section)
    }

    /// Borrowed sections in increasing local-Y order.
    #[must_use]
    pub fn sections(&self) -> &[CompactSection] {
        &self.sections
    }

    /// Consumes the storage into its sections and height. Packed word buffers
    /// can be moved into another section implementation without cell copying.
    #[must_use]
    pub fn into_sections(self) -> (i32, i32, Vec<CompactSection>) {
        (self.min_y, self.height, self.sections)
    }

    /// Palette index at local `(x, y, z)`.
    #[must_use]
    pub fn get(&self, x: usize, y: i32, z: usize) -> u16 {
        assert!(x < 16 && z < 16, "column coordinates out of range");
        let ly = y - self.min_y;
        assert!((0..self.height).contains(&ly), "column Y is out of range");
        let cell = ((ly as usize % SECTION_ROWS) * ROW_CELLS) + z * 16 + x;
        self.sections[ly as usize / SECTION_ROWS].get(cell)
    }

    /// Sets one palette index, widening only the affected section when needed.
    pub fn set(&mut self, x: usize, y: i32, z: usize, id: u16) {
        assert!(x < 16 && z < 16, "column coordinates out of range");
        let ly = y - self.min_y;
        assert!((0..self.height).contains(&ly), "column Y is out of range");
        let section_index = ly as usize / SECTION_ROWS;
        let cell = ((ly as usize % SECTION_ROWS) * ROW_CELLS) + z * 16 + x;
        let rows = self.section_rows(section_index);
        let section = &mut self.sections[section_index];
        match section {
            CompactSection::Uniform(current) => {
                if *current == id {
                    return;
                }
                let old = *current;
                let bits = bits_for_id(old.max(id));
                let per_word = values_per_word(bits);
                let mut words = vec![0u64; packed_word_count(rows * ROW_CELLS, bits)];
                if old != 0 {
                    let mut word = 0u64;
                    for slot in 0..per_word {
                        word |= u64::from(old) << (slot as u32 * bits);
                    }
                    words.fill(word);
                }
                *section = CompactSection::Packed { bits: bits as u8, words };
                crate::counters::bump_full_column_conversion((rows * ROW_CELLS) as u64);
                set_packed(section, cell, id);
            }
            CompactSection::Packed { bits, words } => {
                if bits_for_id(id) > u32::from(*bits) {
                    let wider = bits_for_id(id);
                    let old_bits = u32::from(*bits);
                    let old_per_word = values_per_word(old_bits);
                    let new_per_word = values_per_word(wider);
                    let old_words = std::mem::take(words);
                    let mut next = vec![0u64; packed_word_count(rows * ROW_CELLS, wider)];
                    for cell in 0..rows * ROW_CELLS {
                        let value = (old_words[cell / old_per_word]
                            >> ((cell % old_per_word) as u32 * old_bits))
                            & mask(old_bits);
                        next[cell / new_per_word] |=
                            value << ((cell % new_per_word) as u32 * wider);
                    }
                    *bits = wider as u8;
                    *words = next;
                    crate::counters::bump_full_column_conversion((rows * ROW_CELLS) as u64);
                }
                set_packed(section, cell, id);
            }
        }
    }

    /// Calls `f(cell_in_section, palette_index)` in flat section order.
    pub fn for_each_section(&self, section: usize, mut f: impl FnMut(usize, u16)) {
        let rows = self.section_rows(section);
        if rows == 0 {
            return;
        }
        let count = rows * ROW_CELLS;
        let section_ref = &self.sections[section];
        for cell in 0..count {
            f(cell, section_ref.get(cell));
        }
    }

    /// Appends a section's real cells in flat `(y, z, x)` order.
    pub fn append_section_cells(&self, section: usize, out: &mut Vec<u16>) {
        out.reserve(self.section_rows(section) * ROW_CELLS);
        self.for_each_section(section, |_, id| out.push(id));
    }

    /// Expands the compact field into the compatibility flat layout.
    #[must_use]
    pub fn into_flat(self) -> Vec<u16> {
        let cells = (self.height as usize) * ROW_CELLS;
        let mut flat = Vec::with_capacity(cells);
        for section in 0..self.sections.len() {
            let rows = self.section_rows(section);
            for cell in 0..rows * ROW_CELLS {
                flat.push(self.sections[section].get(cell));
            }
        }
        crate::counters::bump_full_column_conversion(cells as u64);
        flat
    }

    /// Heap bytes owned by the section spine and packed payloads.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.sections.capacity() * std::mem::size_of::<CompactSection>()
            + self.sections.iter().map(CompactSection::heap_bytes).sum::<usize>()
    }

    /// Number of cells that are not palette index zero.
    #[must_use]
    pub fn non_zero_count(&self) -> usize {
        let mut count = 0;
        for section in 0..self.sections.len() {
            self.for_each_section(section, |_, id| count += usize::from(id != 0));
        }
        count
    }
}

/// Vertical products accumulated while the final sections are packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedColumnSummaries {
    non_air_first_free: [u16; 256],
    motion_blocking_first_free: Option<[u16; 256]>,
}

impl GeneratedColumnSummaries {
    fn new(with_motion_blocking: bool) -> Self {
        Self {
            non_air_first_free: [0; 256],
            motion_blocking_first_free: with_motion_blocking.then_some([0; 256]),
        }
    }

    fn observe(
        &mut self,
        section_row: usize,
        cell: usize,
        id: u16,
        motion_blocking: Option<&[bool]>,
    ) {
        let ly = section_row + cell / ROW_CELLS;
        let horizontal = cell % ROW_CELLS;
        let lz = horizontal / 16;
        let lx = horizontal % 16;
        let index = lx + lz * 16;
        let first_free = (ly + 1) as u16;
        if id != 0 {
            self.non_air_first_free[index] = first_free;
        }
        if let (Some(out), Some(predicate)) =
            (&mut self.motion_blocking_first_free, motion_blocking)
        {
            if predicate[id as usize] {
                out[index] = first_free;
            }
        }
    }

    /// Stored first-free row for the highest non-air cell, relative to `min_y`.
    /// Zero means the column has no non-air cell.
    #[must_use]
    pub fn non_air_first_free(&self) -> &[u16; 256] {
        &self.non_air_first_free
    }

    /// Stored first-free row for the highest motion-blocking-or-fluid cell,
    /// relative to `min_y`, when a predicate was supplied.
    #[must_use]
    pub fn motion_blocking_first_free(&self) -> Option<&[u16; 256]> {
        self.motion_blocking_first_free.as_ref()
    }
}

#[inline]
fn bits_for_id(id: u16) -> u32 {
    (u16::BITS - id.leading_zeros()).max(1)
}

#[inline]
fn values_per_word(bits: u32) -> usize {
    VALUES_PER_WORD[bits as usize]
}

#[inline]
fn packed_word_count(values: usize, bits: u32) -> usize {
    values.div_ceil(values_per_word(bits))
}

#[inline]
fn mask(bits: u32) -> u64 {
    (1u64 << bits) - 1
}

#[inline]
fn set_packed(section: &mut CompactSection, cell: usize, id: u16) {
    let CompactSection::Packed { bits, words } = section else {
        unreachable!("set_packed requires a packed section");
    };
    let bits = u32::from(*bits);
    let per_word = values_per_word(bits);
    let shift = (cell % per_word) as u32 * bits;
    let slot = &mut words[cell / per_word];
    *slot = (*slot & !(mask(bits) << shift)) | (u64::from(id) << shift);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(height: usize) -> Vec<u16> {
        (0..height * ROW_CELLS)
            .map(|cell| ((cell.wrapping_mul(37) + cell / SECTION_CELLS) & 0x01ff) as u16)
            .collect()
    }

    #[test]
    fn compact_matches_independent_flat_control_with_negative_min_y() {
        let cells = flat(17);
        let compact = CompactBlockStorage::from_flat(-64, 17, &cells);
        for ly in 0..17 {
            for z in 0..16 {
                for x in 0..16 {
                    let index = (ly * 16 + z) * 16 + x;
                    assert_eq!(compact.get(x, -64 + ly as i32, z), cells[index]);
                }
            }
        }
        assert_eq!(compact.clone().into_flat(), cells);
    }

    #[test]
    fn standard_column_has_exact_flat_cell_count() {
        let cells = vec![0u16; 16 * 384 * 16];
        let compact = CompactBlockStorage::from_flat(-64, 384, &cells);
        assert_eq!(compact.section_count(), 24);
        assert_eq!(compact.clone().into_flat().len(), 16 * 384 * 16);
    }

    #[test]
    fn width_lookup_matches_the_packing_contract_for_every_supported_width() {
        for bits in 1..=u32::from(u16::BITS) {
            assert_eq!(values_per_word(bits), (u64::BITS / bits) as usize);
        }
    }

    #[test]
    fn uniform_and_mixed_sections_have_exact_shapes() {
        let mut cells = vec![0u16; SECTION_CELLS * 2];
        cells[SECTION_CELLS + 3] = 1;
        let compact = CompactBlockStorage::from_flat(0, 32, &cells);
        assert_eq!(compact.section(0).and_then(CompactSection::uniform_id), Some(0));
        assert_eq!(compact.section(1).and_then(CompactSection::uniform_id), None);
        assert_eq!(compact.section(1).map(CompactSection::bits), Some(1));
        assert_eq!(
            compact.heap_bytes(),
            512 + 2 * std::mem::size_of::<CompactSection>()
        );
    }

    #[test]
    fn palette_growth_and_one_block_mutation_preserve_other_cells() {
        let mut cells = vec![0u16; SECTION_CELLS];
        cells[0] = 1;
        cells[1] = 2;
        let mut compact = CompactBlockStorage::from_flat(10, 16, &cells);
        compact.set(7, 10, 9, 255);
        assert_eq!(compact.get(7, 10, 9), 255);
        assert_eq!(compact.get(0, 10, 0), 1);
        assert_eq!(compact.get(1, 10, 0), 2);
        assert_eq!(compact.get(6, 10, 9), 0);
        compact.set(7, 10, 9, 256);
        assert_eq!(compact.get(7, 10, 9), 256);
        assert_eq!(compact.get(0, 10, 0), 1);
        assert_eq!(compact.get(1, 10, 0), 2);
    }

    #[test]
    fn fused_vertical_summaries_match_independent_scalar_controls() {
        let height = 17usize;
        let min_y = -64i32;
        let idx = |ly: usize, lz: usize, lx: usize| (ly * 16 + lz) * 16 + lx;
        let mut cells = vec![0u16; height * ROW_CELLS];
        cells[idx(1, 0, 0)] = 1;
        cells[idx(16, 0, 0)] = 2;
        cells[idx(3, 2, 1)] = 1;
        cells[idx(15, 2, 1)] = 3;
        cells[idx(0, 4, 4)] = 2;
        let motion = [false, true, false, true];

        let (compact, summaries) = CompactBlockStorage::from_flat_with_summaries(
            min_y,
            height as i32,
            &cells,
            Some(&motion),
        );
        let mut non_air = [0u16; 256];
        let mut motion_control = [0u16; 256];
        for ly in 0..height {
            for lz in 0..16 {
                for lx in 0..16 {
                    let id = cells[idx(ly, lz, lx)];
                    let column = lx + lz * 16;
                    if id != 0 {
                        non_air[column] = (ly + 1) as u16;
                    }
                    if motion[id as usize] {
                        motion_control[column] = (ly + 1) as u16;
                    }
                }
            }
        }
        assert_eq!(compact.into_flat(), cells);
        assert_eq!(summaries.non_air_first_free(), &non_air);
        assert_eq!(summaries.motion_blocking_first_free(), Some(&motion_control));
        assert_eq!(summaries.non_air_first_free()[0], 17);
        assert_eq!(summaries.motion_blocking_first_free().unwrap()[0], 2);
        assert_eq!(min_y + i32::from(summaries.non_air_first_free()[0]), -47);
        assert_eq!(summaries.non_air_first_free()[4 + 4 * 16], 1);
    }

    #[test]
    fn fused_summary_negative_controls_change_each_independent_product() {
        let height = 17usize;
        let idx = |ly: usize, lz: usize, lx: usize| (ly * 16 + lz) * 16 + lx;
        let mut cells = vec![0u16; height * ROW_CELLS];
        cells[idx(2, 0, 0)] = 1;
        let motion = [false, true, false];
        let (_, base) = CompactBlockStorage::from_flat_with_summaries(
            -64,
            height as i32,
            &cells,
            Some(&motion),
        );
        let (_, without_motion) = CompactBlockStorage::from_flat_with_summaries(
            -64,
            height as i32,
            &cells,
            None,
        );
        assert_eq!(without_motion.motion_blocking_first_free(), None);

        let mut non_air_change = cells.clone();
        non_air_change[idx(16, 0, 0)] = 2;
        let (_, changed_non_air) = CompactBlockStorage::from_flat_with_summaries(
            -64,
            height as i32,
            &non_air_change,
            Some(&motion),
        );
        assert_ne!(
            base.non_air_first_free()[0],
            changed_non_air.non_air_first_free()[0],
            "the non-air control must detect a changed top cell"
        );

        let mut motion_change = cells.clone();
        motion_change[idx(16, 0, 0)] = 1;
        let (_, changed_motion) = CompactBlockStorage::from_flat_with_summaries(
            -64,
            height as i32,
            &motion_change,
            Some(&motion),
        );
        assert_ne!(
            base.motion_blocking_first_free().unwrap()[0],
            changed_motion.motion_blocking_first_free().unwrap()[0],
            "the motion-blocking control must detect a changed top cell"
        );
    }
}
