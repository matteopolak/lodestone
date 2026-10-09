//! Section-aligned block-index storage for generated columns.
//!
//! [`CompactBlockStorage`] keeps one column-wide palette index space while
//! representing each 16-row section as either one repeated value or a packed
//! `u16` index stream. The representation is deliberately independent of block
//! names and registries: the palette belongs to the generated column, and this
//! type only owns its indices.

use std::sync::Arc;

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

}

/// A generated column's compact block-index field.
///
/// Sections sit behind one `Arc`, so a clone is a snapshot that shares every
/// packed payload until either side writes.
#[derive(Debug, Clone)]
pub struct CompactBlockStorage {
    min_y: i32,
    height: i32,
    sections: Arc<Vec<CompactSection>>,
}

/// Equality is by cell contents: a section widened by a write and one packed
/// narrow from the start compare equal when every cell agrees.
impl PartialEq for CompactBlockStorage {
    fn eq(&self, other: &Self) -> bool {
        self.min_y == other.min_y
            && self.height == other.height
            && (0..self.height as usize * ROW_CELLS)
                .all(|index| self.cell_at_flat(index) == other.cell_at_flat(index))
    }
}

impl Eq for CompactBlockStorage {}

impl CompactBlockStorage {
    /// A column whose every cell is palette index `id`. Allocates no payload.
    #[must_use]
    pub fn uniform(min_y: i32, height: i32, id: u16) -> Self {
        assert!(height >= 0, "column height is negative");
        Self {
            min_y,
            height,
            sections: Arc::new(vec![
                CompactSection::Uniform(id);
                (height as usize).div_ceil(SECTION_ROWS)
            ]),
        }
    }

    /// Packs a column from one reusable section buffer. The callback returns
    /// a proven uniform index, or fills every real cell and returns `None`.
    /// Uniform summaries use section arithmetic without walking the cells.
    /// Disabling summaries packs storage without allocating or observing metadata.
    #[must_use]
    pub fn from_section_fn_with_predicates(
        min_y: i32,
        height: i32,
        palette_len: usize,
        with_summaries: bool,
        motion_blocking: Option<&[bool]>,
        motion_blocking_no_leaves: Option<&[bool]>,
        extra_air: [u16; 2],
        mut fill_section: impl FnMut(usize, &mut [u16]) -> Option<u16>,
    ) -> (Self, Option<GeneratedColumnSummaries>) {
        assert!(height >= 0, "column height is negative");
        assert!(with_summaries || (motion_blocking.is_none() && motion_blocking_no_leaves.is_none()),
            "heightmap predicates require summaries");
        let mut summaries = with_summaries.then(|| GeneratedColumnSummaries::new(
            height,
            palette_len.max(1),
            motion_blocking.is_some(),
            motion_blocking_no_leaves.is_some(),
        ));
        let mut scratch = [0u16; SECTION_CELLS];
        let section_count = (height as usize).div_ceil(SECTION_ROWS);
        let mut packed = Vec::with_capacity(section_count);
        let mut observed_cells = 0;
        for section in 0..section_count {
            let rows = (height as usize - section * SECTION_ROWS).min(SECTION_ROWS);
            let cells = &mut scratch[..rows * ROW_CELLS];
            if let Some(id) = fill_section(section, cells) {
                if let Some(summaries) = &mut summaries {
                    summaries.observe_uniform(
                        section, rows, id, motion_blocking, motion_blocking_no_leaves, extra_air,
                    );
                }
                packed.push(CompactSection::Uniform(id));
                continue;
            }
            if let Some(summaries) = &mut summaries {
                observed_cells += cells.len() as u64;
                packed.push(CompactSection::pack_observed(cells, rows, |cell, id| {
                    summaries.observe(
                        section * SECTION_ROWS,
                        cell,
                        id,
                        motion_blocking,
                        motion_blocking_no_leaves,
                        extra_air,
                    );
                }));
            } else {
                packed.push(CompactSection::pack(cells, rows));
            }
        }
        crate::counters::bump_raw_window(0, observed_cells, section_count as u64);
        crate::counters::bump_full_column_conversion(height as u64 * ROW_CELLS as u64);
        (Self { min_y, height, sections: Arc::new(packed) }, summaries)
    }

    #[inline]
    fn cell_at_flat(&self, index: usize) -> u16 {
        self.sections[index / SECTION_CELLS].get(index % SECTION_CELLS)
    }

    /// World Y origin of this storage.
    #[must_use]
    pub const fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of 16-row sections, rounded up for a partial top section.
    #[must_use]
    pub fn section_count(&self) -> usize {
        (self.height as usize).div_ceil(SECTION_ROWS)
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

    /// Palette index at local `(x, y, z)`.
    #[inline]
    #[must_use]
    pub fn get(&self, x: usize, y: i32, z: usize) -> u16 {
        assert!(x < 16 && z < 16, "column coordinates out of range");
        let ly = y - self.min_y;
        assert!((0..self.height).contains(&ly), "column Y is out of range");
        self.cell_at_flat((ly as usize * 16 + z) * 16 + x)
    }

    /// Sets one palette index, widening only the affected section when needed.
    /// Detaches from any snapshot sharing the sections first.
    #[inline]
    pub fn set(&mut self, x: usize, y: i32, z: usize, id: u16) {
        assert!(x < 16 && z < 16, "column coordinates out of range");
        let ly = y - self.min_y;
        assert!((0..self.height).contains(&ly), "column Y is out of range");
        let section_index = ly as usize / SECTION_ROWS;
        let cell = ((ly as usize % SECTION_ROWS) * ROW_CELLS) + z * 16 + x;
        let rows = self.section_rows(section_index);
        let sections = Arc::make_mut(&mut self.sections);
        Self::set_compact_section(&mut sections[section_index], rows, cell, id);
    }

    fn set_compact_section(section: &mut CompactSection, rows: usize, cell: usize, id: u16) {
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
        let section_ref = &self.sections[section];
        for cell in 0..rows * ROW_CELLS {
            f(cell, section_ref.get(cell));
        }
    }

    /// Appends a section's real cells in flat `(y, z, x)` order.
    pub fn append_section_cells(&self, section: usize, out: &mut Vec<u16>) {
        out.reserve(self.section_rows(section) * ROW_CELLS);
        self.for_each_section(section, |_, id| out.push(id));
    }

    /// Heap bytes owned by the section spine and packed payloads.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.sections.capacity() * std::mem::size_of::<CompactSection>()
            + self.sections.iter().map(CompactSection::heap_bytes).sum::<usize>()
    }
}

/// Vertical products accumulated while the final sections are packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedColumnSummaries {
    non_air_first_free: [u16; 256],
    motion_blocking_first_free: Option<[u16; 256]>,
    motion_blocking_no_leaves_first_free: Option<[u16; 256]>,
    section_state_counts: Vec<Vec<u16>>,
}

impl GeneratedColumnSummaries {
    fn new(
        height: i32,
        palette_len: usize,
        with_motion_blocking: bool,
        with_motion_blocking_no_leaves: bool,
    ) -> Self {
        Self {
            non_air_first_free: [0; 256],
            motion_blocking_first_free: with_motion_blocking.then_some([0; 256]),
            motion_blocking_no_leaves_first_free:
                with_motion_blocking_no_leaves.then_some([0; 256]),
            section_state_counts: vec![vec![0; palette_len]; (height as usize).div_ceil(SECTION_ROWS)],
        }
    }

    fn observe_uniform(
        &mut self,
        section: usize,
        rows: usize,
        id: u16,
        motion_blocking: Option<&[bool]>,
        motion_blocking_no_leaves: Option<&[bool]>,
        extra_air: [u16; 2],
    ) {
        self.section_state_counts[section][id as usize] += (rows * ROW_CELLS) as u16;
        let first_free = (section * SECTION_ROWS + rows) as u16;
        if id != 0 && id != extra_air[0] && id != extra_air[1] {
            self.non_air_first_free.fill(first_free);
        }
        if let (Some(out), Some(predicate)) = (&mut self.motion_blocking_first_free, motion_blocking) {
            if predicate[id as usize] {
                out.fill(first_free);
            }
        }
        if let (Some(out), Some(predicate)) = (
            &mut self.motion_blocking_no_leaves_first_free, motion_blocking_no_leaves,
        ) {
            if predicate[id as usize] {
                out.fill(first_free);
            }
        }
    }

    fn observe(
        &mut self,
        section_row: usize,
        cell: usize,
        id: u16,
        motion_blocking: Option<&[bool]>,
        motion_blocking_no_leaves: Option<&[bool]>,
        extra_air: [u16; 2],
    ) {
        let ly = section_row + cell / ROW_CELLS;
        self.section_state_counts[ly / SECTION_ROWS][id as usize] += 1;
        let index = cell % ROW_CELLS;
        let first_free = (ly + 1) as u16;
        if id != 0 && id != extra_air[0] && id != extra_air[1] {
            self.non_air_first_free[index] = first_free;
        }
        if let (Some(out), Some(predicate)) =
            (&mut self.motion_blocking_first_free, motion_blocking)
        {
            if predicate[id as usize] {
                out[index] = first_free;
            }
        }
        if let (Some(out), Some(predicate)) = (
            &mut self.motion_blocking_no_leaves_first_free,
            motion_blocking_no_leaves,
        ) {
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

    /// Stored first-free row for motion-blocking cells excluding leaves.
    #[must_use]
    pub fn motion_blocking_no_leaves_first_free(&self) -> Option<&[u16; 256]> {
        self.motion_blocking_no_leaves_first_free.as_ref()
    }

    /// Per-section counts of each generated palette index. This sidecar is
    /// consumed once by the server and is not retained in the final column.
    #[must_use]
    pub fn section_state_counts(&self) -> &[Vec<u16>] {
        &self.section_state_counts
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

    /// Packs a flat `((ly * 16 + z) * 16 + x)` column through the production
    /// section callback, never claiming a section is uniform.
    fn pack(
        min_y: i32,
        height: i32,
        cells: &[u16],
        motion_blocking: Option<&[bool]>,
        motion_blocking_no_leaves: Option<&[bool]>,
        extra_air: [u16; 2],
    ) -> (CompactBlockStorage, GeneratedColumnSummaries) {
        let palette_len = cells.iter().copied().max().map_or(1, |id| usize::from(id) + 1);
        let (storage, summaries) = CompactBlockStorage::from_section_fn_with_predicates(
            min_y, height, palette_len, true, motion_blocking, motion_blocking_no_leaves,
            extra_air,
            |section, target| {
                let start = section * SECTION_CELLS;
                target.copy_from_slice(&cells[start..start + target.len()]);
                None
            },
        );
        (storage, summaries.expect("summaries requested"))
    }

    fn from_flat(min_y: i32, height: i32, cells: &[u16]) -> CompactBlockStorage {
        pack(min_y, height, cells, None, None, [u16::MAX; 2]).0
    }

    fn to_flat(storage: &CompactBlockStorage) -> Vec<u16> {
        let mut out = Vec::new();
        for section in 0..storage.section_count() {
            storage.append_section_cells(section, &mut out);
        }
        out
    }

    /// First-free rows computed by a plain scan over the flat layout, sharing no
    /// code with the observer under test.
    fn scalar_first_free(height: usize, cells: &[u16], hit: impl Fn(u16) -> bool) -> [u16; 256] {
        let mut out = [0u16; 256];
        for ly in 0..height {
            for column in 0..256 {
                if hit(cells[ly * 256 + column]) {
                    out[column] = (ly + 1) as u16;
                }
            }
        }
        out
    }

    #[test]
    fn compact_matches_independent_flat_control_with_negative_min_y() {
        let cells = flat(17);
        let compact = from_flat(-64, 17, &cells);
        for ly in 0..17 {
            for z in 0..16 {
                for x in 0..16 {
                    let index = (ly * 16 + z) * 16 + x;
                    assert_eq!(compact.get(x, -64 + ly as i32, z), cells[index]);
                }
            }
        }
        assert_eq!(to_flat(&compact), cells);
    }

    #[test]
    fn uniform_section_summaries_match_scalar_control_with_zero_predicate_and_partial_rows() {
        let ids = [1, 0, 2];
        let motion = [true, false, false];
        let no_leaves = [false, true, false];
        let extra_air = [2, u16::MAX];
        let mut cells = vec![1; 16 * ROW_CELLS];
        cells.extend(vec![0; 16 * ROW_CELLS]);
        cells.extend(vec![2; 3 * ROW_CELLS]);
        let (storage, actual) = CompactBlockStorage::from_section_fn_with_predicates(
            -37, 35, 3, true, Some(&motion), Some(&no_leaves), extra_air,
            |section, _| Some(ids[section]),
        );
        let actual = actual.expect("summaries requested");
        let (_, walked) = pack(-37, 35, &cells, Some(&motion), Some(&no_leaves), extra_air);
        assert_eq!(actual, walked, "the uniform shortcut must match walking every cell");
        assert_eq!(
            actual.non_air_first_free(),
            &scalar_first_free(35, &cells, |id| id != 0 && id != 2),
        );
        assert_eq!(actual.non_air_first_free(), &[16; ROW_CELLS]);
        assert_eq!(actual.motion_blocking_first_free(), Some(&[32; ROW_CELLS]));
        assert_eq!(actual.motion_blocking_no_leaves_first_free(), Some(&[16; ROW_CELLS]));
        assert_eq!(actual.section_state_counts(), &[vec![0, 4096, 0], vec![4096, 0, 0], vec![0, 0, 768]]);
        for (section, id) in ids.into_iter().enumerate() {
            assert_eq!(storage.section(section).unwrap().uniform_id(), Some(id));
        }
        assert_eq!(to_flat(&storage), cells);
        let (_, without_predicates) = CompactBlockStorage::from_section_fn_with_predicates(
            -37, 35, 3, true, None, None, extra_air, |section, _| Some(ids[section]),
        );
        let without_predicates = without_predicates.expect("histogram requested");
        assert_eq!(without_predicates.non_air_first_free(), actual.non_air_first_free());
        assert!(without_predicates.motion_blocking_first_free().is_none());
        assert!(without_predicates.motion_blocking_no_leaves_first_free().is_none());
        let (_, wrong) = pack(-37, 35, &cells, Some(&[false; 3]), Some(&no_leaves), extra_air);
        assert_ne!(actual.motion_blocking_first_free(), wrong.motion_blocking_first_free());
        let (without_metadata, omitted) = CompactBlockStorage::from_section_fn_with_predicates(
            -37, 35, usize::MAX, false, None, None, extra_air,
            |section, target| {
                let start = section * SECTION_CELLS;
                target.copy_from_slice(&cells[start..start + target.len()]);
                None
            },
        );
        assert!(omitted.is_none());
        assert_eq!(to_flat(&without_metadata), cells);
    }

    #[test]
    fn standard_column_has_exact_flat_cell_count() {
        let cells = vec![0u16; 16 * 384 * 16];
        let compact = from_flat(-64, 384, &cells);
        assert_eq!(compact.section_count(), 24);
        assert_eq!(to_flat(&compact).len(), 16 * 384 * 16);
    }

    #[test]
    fn uniform_partial_column_keeps_its_shared_snapshot_on_write() {
        let mut storage = CompactBlockStorage::uniform(-64, 17, 9);
        assert_eq!(storage.section_count(), 2);
        assert_eq!(storage.section_rows(1), 1);
        let snapshot = storage.clone();
        assert!(Arc::ptr_eq(&storage.sections, &snapshot.sections));
        storage.set(3, -48, 7, 18);
        assert_eq!(storage.get(3, -48, 7), 18);
        assert_eq!(snapshot.get(3, -48, 7), 9);
        assert_eq!(storage.get(15, -64, 15), 9);
        assert!(!Arc::ptr_eq(&storage.sections, &snapshot.sections));
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
        let compact = from_flat(0, 32, &cells);
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
        let mut compact = from_flat(10, 16, &cells);
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

        let (compact, summaries) =
            pack(min_y, height as i32, &cells, Some(&motion), None, [u16::MAX; 2]);
        assert_eq!(to_flat(&compact), cells);
        assert_eq!(summaries.non_air_first_free(), &scalar_first_free(height, &cells, |id| id != 0));
        assert_eq!(
            summaries.motion_blocking_first_free(),
            Some(&scalar_first_free(height, &cells, |id| motion[id as usize])),
        );
        assert!(summaries.motion_blocking_no_leaves_first_free().is_none());
        let mut state_counts = vec![vec![0u16; 4]; 2];
        for (index, &id) in cells.iter().enumerate() {
            state_counts[index / SECTION_CELLS][id as usize] += 1;
        }
        assert_eq!(summaries.section_state_counts(), state_counts.as_slice());
        assert_eq!(summaries.non_air_first_free()[0], 17);
        assert_eq!(summaries.motion_blocking_first_free().unwrap()[0], 2);
        assert_eq!(min_y + i32::from(summaries.non_air_first_free()[0]), -47);
        assert_eq!(summaries.non_air_first_free()[4 + 4 * 16], 1);
    }

    #[test]
    fn summary_excludes_nondefault_air_palette_entries() {
        let mut cells = vec![0u16; 3 * ROW_CELLS];
        cells[0] = 1;
        cells[ROW_CELLS] = 2;
        cells[2 * ROW_CELLS] = 3;
        let (_, wrong) = pack(-64, 3, &cells, None, None, [u16::MAX; 2]);
        let (_, summaries) = pack(-64, 3, &cells, None, None, [2, 3]);
        assert_eq!(wrong.non_air_first_free()[0], 3);
        assert_eq!(summaries.non_air_first_free()[0], 1);
        assert_eq!(summaries.non_air_first_free()[1], 0);
    }

    #[test]
    fn fused_summary_negative_controls_change_each_independent_product() {
        let height = 17usize;
        let idx = |ly: usize, lz: usize, lx: usize| (ly * 16 + lz) * 16 + lx;
        let mut cells = vec![0u16; height * ROW_CELLS];
        cells[idx(2, 0, 0)] = 1;
        let motion = [false, true, false];
        let summarise = |cells: &[u16], motion: Option<&[bool]>| {
            pack(-64, height as i32, cells, motion, None, [u16::MAX; 2]).1
        };
        let base = summarise(&cells, Some(&motion));
        assert_eq!(summarise(&cells, None).motion_blocking_first_free(), None);

        let mut non_air_change = cells.clone();
        non_air_change[idx(16, 0, 0)] = 2;
        assert_ne!(
            base.non_air_first_free()[0],
            summarise(&non_air_change, Some(&motion)).non_air_first_free()[0],
            "the non-air control must detect a changed top cell"
        );

        let mut motion_change = cells.clone();
        motion_change[idx(16, 0, 0)] = 1;
        assert_ne!(
            base.motion_blocking_first_free().unwrap()[0],
            summarise(&motion_change, Some(&motion)).motion_blocking_first_free().unwrap()[0],
            "the motion-blocking control must detect a changed top cell"
        );
    }
}
