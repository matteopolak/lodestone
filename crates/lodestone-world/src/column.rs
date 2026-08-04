//! A vertical stack of chunk sections over a configurable height range.

use std::sync::Arc;

use crate::container::{PaletteKind, PalettedContainer};
use crate::section::ChunkSection;

/// A full-height column of chunk sections at a fixed `(x, z)`.
///
/// The height range is configurable because world height is version-dependent:
/// 1.18+ overworld runs `y = -64..320` (min-Y `-64`, 24 sections) while legacy
/// worlds run `0..256` (min-Y `0`, 16 sections). The number of sections is a
/// constructor parameter and never hardcoded.
///
/// Sections that hold nothing but air (and the default biome) are stored as
/// `None` rather than an allocated zeroed section, so a freshly created or
/// mostly-empty column costs almost nothing.
///
/// Each present section is held behind an [`Arc`] so that a block update is
/// copy-on-write at *section* granularity: mutating one section forks only that
/// section (and only while a snapshot is outstanding), never the whole column.
/// This matches the mesher's access unit — it clones the section `Arc`s of a
/// 3×3×3 neighbourhood, drops any lock immediately, and meshes off a stable
/// snapshot while loads and edits continue without copying section bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkColumn {
    min_y: i32,
    block_kind: PaletteKind,
    biome_kind: PaletteKind,
    air_id: u32,
    biome_id: u32,
    sections: Vec<Option<Arc<ChunkSection>>>,
}

impl ChunkColumn {
    /// Creates an all-air column of `section_count` sections whose lowest block
    /// is at world-`y` `min_y`.
    ///
    /// `air_id` is the block-state id treated as air and `biome_id` is the
    /// default biome id; a section equal to both is considered empty and elided.
    #[must_use]
    pub fn new(
        min_y: i32,
        section_count: usize,
        block_kind: PaletteKind,
        biome_kind: PaletteKind,
        air_id: u32,
        biome_id: u32,
    ) -> Self {
        Self {
            min_y,
            block_kind,
            biome_kind,
            air_id,
            biome_id,
            sections: (0..section_count).map(|_| None).collect(),
        }
    }

    /// Lowest world-`y` in the column.
    #[must_use]
    pub const fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Highest world-`y` in the column, exclusive.
    #[must_use]
    pub fn max_y(&self) -> i32 {
        self.min_y + (self.sections.len() * ChunkSection::EDGE) as i32
    }

    /// Number of sections in the column.
    #[must_use]
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }

    /// The block-state id treated as air.
    #[must_use]
    pub const fn air_id(&self) -> u32 {
        self.air_id
    }

    /// Section index for world-`y`, or `None` if `y` is outside the column.
    #[must_use]
    pub fn section_index(&self, y: i32) -> Option<usize> {
        if y < self.min_y || y >= self.max_y() {
            return None;
        }
        Some(((y - self.min_y) / ChunkSection::EDGE as i32) as usize)
    }

    /// Borrows the section at `section_index`, if present (allocated).
    #[must_use]
    pub fn section(&self, section_index: usize) -> Option<&ChunkSection> {
        self.sections.get(section_index).and_then(|s| s.as_deref())
    }

    /// Returns an owned, lock-free clone of the section `Arc` at `section_index`,
    /// if present.
    ///
    /// This is the mesher's entry point: it bumps a refcount rather than copying
    /// section bytes, so the caller can drop any world lock and mesh off a stable
    /// snapshot while the column keeps loading and mutating. A later edit of that
    /// section forks it copy-on-write, leaving this snapshot untouched.
    #[must_use]
    pub fn section_arc(&self, section_index: usize) -> Option<Arc<ChunkSection>> {
        self.sections.get(section_index).and_then(Clone::clone)
    }

    /// Returns the block state at world `(x, y, z)`.
    ///
    /// `x` and `z` are the in-chunk coordinates `0..16`. Returns air for any
    /// `y` outside the column or in an elided (all-air) section.
    ///
    /// # Panics
    /// Panics if `x` or `z` is out of range.
    #[must_use]
    pub fn get_block(&self, x: usize, y: i32, z: usize) -> u32 {
        match self.section_index(y) {
            Some(idx) => match &self.sections[idx] {
                Some(section) => section.get_block(x, local_y(y, self.min_y), z),
                None => self.air_id,
            },
            None => self.air_id,
        }
    }

    /// Sets the block state at world `(x, y, z)`.
    ///
    /// Allocates the target section on demand, and elides it back to `None` if
    /// the write leaves the section empty (all air and default biome).
    ///
    /// # Panics
    /// Panics if `x` or `z` is out of range, or `y` is outside the column.
    pub fn set_block(&mut self, x: usize, y: i32, z: usize, value: u32) {
        let idx = self
            .section_index(y)
            .expect("y coordinate outside column height range");
        let ly = local_y(y, self.min_y);

        if self.sections[idx].is_none() {
            if value == self.air_id {
                return;
            }
            self.sections[idx] = Some(Arc::new(self.empty_section()));
        }

        let section = Arc::make_mut(self.sections[idx].as_mut().expect("just ensured present"));
        section.set_block(x, ly, z, value);

        if section.is_empty(self.biome_id) {
            self.sections[idx] = None;
        }
    }

    /// Applies many block writes to a single section, forking that section's
    /// `Arc` at most **once** regardless of how many blocks change.
    ///
    /// This is the bulk path a `section_blocks_update` packet routes into: the
    /// packet carries many positions within one section, so a naive loop of
    /// [`set_block`](Self::set_block) would `Arc::make_mut` (and possibly clone)
    /// the section on every entry. Here the section is resolved and made mutable
    /// once, then every `(x, y, z, state)` in `entries` — section-relative
    /// coordinates in `0..16` — is applied.
    ///
    /// The section is allocated on demand only if some entry is non-air, and is
    /// elided back to `None` if the batch leaves it empty. A `section_index`
    /// outside the column is ignored.
    ///
    /// # Panics
    /// Panics if any entry coordinate is outside `0..16`.
    pub fn set_blocks_in_section(&mut self, section_index: usize, entries: &[(u8, u8, u8, u32)]) {
        if entries.is_empty() || section_index >= self.sections.len() {
            return;
        }
        if self.sections[section_index].is_none() {
            if entries.iter().all(|&(_, _, _, v)| v == self.air_id) {
                return;
            }
            self.sections[section_index] = Some(Arc::new(self.empty_section()));
        }

        let section = Arc::make_mut(
            self.sections[section_index]
                .as_mut()
                .expect("just ensured present"),
        );
        for &(x, y, z, value) in entries {
            section.set_block(x as usize, y as usize, z as usize, value);
        }

        if section.is_empty(self.biome_id) {
            self.sections[section_index] = None;
        }
    }

    /// Returns the biome at world biome cell `(x, y, z)`.
    ///
    /// `x` and `z` are in-chunk biome cells `0..4`; `y` is a world-`y` block
    /// coordinate that is floored to its 4×4×4 biome cell.
    ///
    /// # Panics
    /// Panics if `x` or `z` is out of range.
    #[must_use]
    pub fn get_biome(&self, x: usize, y: i32, z: usize) -> u32 {
        match self.section_index(y) {
            Some(idx) => match &self.sections[idx] {
                Some(section) => section.get_biome(x, local_y(y, self.min_y) / 4, z),
                None => self.biome_id,
            },
            None => self.biome_id,
        }
    }

    /// Sets the biome at world biome cell `(x, y, z)`.
    ///
    /// # Panics
    /// Panics if `x` or `z` is out of range, or `y` is outside the column.
    pub fn set_biome(&mut self, x: usize, y: i32, z: usize, value: u32) {
        let idx = self
            .section_index(y)
            .expect("y coordinate outside column height range");
        let ly = local_y(y, self.min_y) / 4;

        if self.sections[idx].is_none() {
            if value == self.biome_id {
                return;
            }
            self.sections[idx] = Some(Arc::new(self.empty_section()));
        }

        let section = Arc::make_mut(self.sections[idx].as_mut().expect("just ensured present"));
        section.set_biome(x, ly, z, value);

        if section.is_empty(self.biome_id) {
            self.sections[idx] = None;
        }
    }

    /// Replaces the whole biome container of the section at `section_index`,
    /// leaving block state (and every other section) untouched.
    ///
    /// This is [`set_biome`](Self::set_biome)'s whole-container counterpart: a
    /// `chunks_biomes` update carries no block data at all, only one biome
    /// container per section, so applying it must not touch `block_states`.
    /// Allocates the section on demand if absent (unless the new biome
    /// container is all-default, matching [`set_biome`](Self::set_biome)'s own
    /// early-out) and elides it back to `None` if the write leaves it empty.
    ///
    /// # Panics
    /// Panics if `section_index >= section_count()`, or `biomes.entry_count() != 64`.
    pub fn set_biome_section(&mut self, section_index: usize, biomes: PalettedContainer) {
        assert!(
            section_index < self.sections.len(),
            "section index outside column height range"
        );
        if self.sections[section_index].is_none() {
            if biomes.single_value() == Some(self.biome_id) {
                return;
            }
            self.sections[section_index] = Some(Arc::new(self.empty_section()));
        }

        let section =
            Arc::make_mut(self.sections[section_index].as_mut().expect("just ensured present"));
        section.set_biomes(biomes);

        if section.is_empty(self.biome_id) {
            self.sections[section_index] = None;
        }
    }

    /// Replaces the section at `section_index`, or clears it with `None`.
    ///
    /// The section is moved behind an [`Arc`] internally; the version crate that
    /// decodes a chunk passes plain `ChunkSection`s and never sees the sharing.
    ///
    /// # Panics
    /// Panics if `section_index >= section_count()`.
    pub fn set_section(&mut self, section_index: usize, section: Option<ChunkSection>) {
        self.sections[section_index] = section.map(Arc::new);
    }

    /// Number of allocated (present) sections.
    #[must_use]
    pub fn allocated_sections(&self) -> usize {
        self.sections.iter().filter(|s| s.is_some()).count()
    }

    /// Total heap bytes owned by the column: the section pointer vector plus, for
    /// every allocated section, its `Arc` allocation (control block + the moved
    /// `ChunkSection` struct) and that section's own container heap.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        let vec_bytes =
            self.sections.capacity() * core::mem::size_of::<Option<Arc<ChunkSection>>>();
        let section_bytes: usize = self
            .sections
            .iter()
            .flatten()
            .map(|s| {
                // Arc strong+weak counters + the ChunkSection struct moved onto
                // the heap + the section's own container allocations.
                2 * core::mem::size_of::<usize>()
                    + core::mem::size_of::<ChunkSection>()
                    + s.heap_bytes()
            })
            .sum();
        vec_bytes + section_bytes
    }

    fn empty_section(&self) -> ChunkSection {
        ChunkSection::new(self.block_kind, self.biome_kind, self.air_id, self.biome_id)
    }
}

/// Local `y` within a section (`0..16`) for a world-`y`.
fn local_y(y: i32, min_y: i32) -> usize {
    (y - min_y).rem_euclid(ChunkSection::EDGE as i32) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modern() -> ChunkColumn {
        // 1.18+ overworld: y = -64..320, 24 sections.
        ChunkColumn::new(
            -64,
            24,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            0,
            0,
        )
    }

    fn legacy() -> ChunkColumn {
        // Pre-1.18: y = 0..256, 16 sections.
        ChunkColumn::new(
            0,
            16,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            0,
            0,
        )
    }

    #[test]
    fn configurable_height_ranges() {
        let m = modern();
        assert_eq!(m.min_y(), -64);
        assert_eq!(m.max_y(), 320);
        assert_eq!(m.section_count(), 24);
        assert_eq!(m.section_index(-64), Some(0));
        assert_eq!(m.section_index(319), Some(23));
        assert_eq!(m.section_index(-65), None);
        assert_eq!(m.section_index(320), None);

        let l = legacy();
        assert_eq!(l.min_y(), 0);
        assert_eq!(l.max_y(), 256);
        assert_eq!(l.section_count(), 16);
        assert_eq!(l.section_index(0), Some(0));
        assert_eq!(l.section_index(255), Some(15));
        assert_eq!(l.section_index(256), None);
    }

    #[test]
    fn all_air_column_allocates_no_section_storage() {
        let c = modern();
        assert_eq!(c.allocated_sections(), 0);
        for i in 0..c.section_count() {
            assert!(c.section(i).is_none(), "section {i} must be absent");
        }
        // Reads anywhere return air without allocating.
        assert_eq!(c.get_block(0, 100, 0), 0);
        assert_eq!(c.get_block(15, -64, 15), 0);
    }

    #[test]
    fn setting_air_on_empty_section_stays_elided() {
        let mut c = modern();
        c.set_block(0, 70, 0, 0); // air over air
        assert_eq!(c.allocated_sections(), 0);
    }

    #[test]
    fn set_and_elide_round_trip() {
        let mut c = modern();
        // Place a non-air block near y=70; that section becomes allocated.
        c.set_block(3, 70, 5, 12);
        assert_eq!(c.get_block(3, 70, 5), 12);
        assert_eq!(c.allocated_sections(), 1);

        // Removing it (back to air) elides the section again.
        c.set_block(3, 70, 5, 0);
        assert_eq!(c.get_block(3, 70, 5), 0);
        assert_eq!(c.allocated_sections(), 0, "empty section must be elided");
    }

    #[test]
    fn blocks_land_in_the_right_section() {
        let mut c = modern();
        // World y = -64 is section 0, y = 0 is section 4, y = 300 is section 22.
        c.set_block(0, -64, 0, 1);
        c.set_block(0, 0, 0, 2);
        c.set_block(0, 300, 0, 3);
        assert_eq!(c.get_block(0, -64, 0), 1);
        assert_eq!(c.get_block(0, 0, 0), 2);
        assert_eq!(c.get_block(0, 300, 0), 3);
        assert_eq!(c.allocated_sections(), 3);
        assert!(c.section(0).is_some());
        assert!(c.section(4).is_some());
        assert!(c.section(22).is_some());
    }

    #[test]
    fn biome_write_allocates_and_reads_back() {
        let mut c = legacy();
        c.set_biome(0, 40, 0, 7);
        assert_eq!(c.get_biome(0, 40, 0), 7);
        assert_eq!(c.allocated_sections(), 1);
        // The block half is still all air.
        assert!(c.section(2).unwrap().is_air_only());
    }

    // --- `set_biome_section`: the `chunks_biomes` write path (issue #26) ---

    #[test]
    fn set_biome_section_replaces_biomes_without_touching_blocks() {
        let mut c = legacy();
        // Give the target section real block data first. World y = 32 is
        // section 2's first block row *and* biome cell row (both floor to
        // local 0), so a raw container index 0 and a `get_biome(_, 32, _)`
        // read agree on which cell they mean.
        c.set_block(3, 32, 5, 12);
        assert_eq!(c.get_block(3, 32, 5), 12);

        let mut biomes = PalettedContainer::new(PaletteKind::biomes(), 0);
        biomes.set(0, 9);
        c.set_biome_section(2, biomes);

        assert_eq!(c.get_biome(0, 32, 0), 9, "biome cell (0,0,0) now holds 9");
        assert_eq!(
            c.get_block(3, 32, 5),
            12,
            "block write from before the biome-only update must survive it"
        );
    }

    #[test]
    fn set_biome_section_on_an_absent_section_allocates_only_for_a_non_default_biome() {
        let mut c = legacy();
        assert_eq!(c.allocated_sections(), 0);

        // An all-default biome container over an absent (all-air, all-default)
        // section must stay elided — this is the same early-out `set_biome`
        // already has, just at whole-container granularity.
        let default_biomes = PalettedContainer::new(PaletteKind::biomes(), 0);
        c.set_biome_section(2, default_biomes);
        assert_eq!(c.allocated_sections(), 0, "an all-default write stays elided");

        let mut non_default = PalettedContainer::new(PaletteKind::biomes(), 0);
        non_default.set(0, 3);
        c.set_biome_section(2, non_default);
        assert_eq!(c.allocated_sections(), 1);
        assert_eq!(c.get_biome(0, 32, 0), 3);
        assert!(
            c.section(2).unwrap().is_air_only(),
            "the allocated section still has default (air) blocks"
        );
    }

    #[test]
    fn set_biome_section_elides_a_section_the_write_leaves_empty() {
        let mut c = legacy();
        let mut biomes = PalettedContainer::new(PaletteKind::biomes(), 0);
        biomes.set(0, 5);
        c.set_biome_section(2, biomes);
        assert_eq!(c.allocated_sections(), 1);

        // Writing the all-default container back over it must elide the
        // section again, exactly as `set_biome` does one cell at a time.
        let all_default = PalettedContainer::new(PaletteKind::biomes(), 0);
        c.set_biome_section(2, all_default);
        assert_eq!(c.allocated_sections(), 0);
    }

    #[test]
    #[should_panic(expected = "biome container must hold 64 entries")]
    fn set_biome_section_rejects_a_wrong_sized_container() {
        let mut c = legacy();
        // Force the section to already be allocated, so the wrong-sized
        // container reaches `ChunkSection::set_biomes` rather than being
        // short-circuited by the absent-section early-out above.
        c.set_biome(0, 40, 0, 1);
        // A block-state-shaped container (4096 entries) must be rejected rather
        // than silently truncated or panicking somewhere less legible.
        let wrong = PalettedContainer::new(PaletteKind::block_states(), 0);
        c.set_biome_section(2, wrong);
    }

    // --- Section-granularity Arc / copy-on-write (§12.37) ---
    //
    // The load-bearing invariant: a block update rebuilds exactly ONE section,
    // never a whole column, and a snapshot handed to a mesher stays stable
    // across concurrent edits. `section_arc` hands out an owned, lock-free
    // clone of one section's `Arc`.

    #[test]
    fn set_block_is_copy_on_write_when_a_snapshot_is_held() {
        let mut c = modern();
        c.set_block(0, -64, 0, 5); // allocate + write section 0
        let held = c.section_arc(0).expect("section 0 present");
        assert_eq!(held.get_block(0, 0, 0), 5);

        // A second edit while the snapshot is held must fork, not mutate it.
        c.set_block(0, -64, 0, 9);
        assert_eq!(held.get_block(0, 0, 0), 5, "held snapshot is COW-isolated");
        assert_eq!(c.get_block(0, -64, 0), 9, "column reflects the new write");
        assert!(
            !Arc::ptr_eq(&held, &c.section_arc(0).unwrap()),
            "the shared section was forked, not mutated in place"
        );
    }

    #[test]
    fn set_block_edits_in_place_when_section_is_unshared() {
        let mut c = modern();
        c.set_block(0, -64, 0, 1);
        let before = Arc::as_ptr(&c.section_arc(0).unwrap());
        // No outstanding snapshot -> refcount 1 -> mutate in place, no clone.
        c.set_block(1, -64, 1, 2);
        let after = Arc::as_ptr(&c.section_arc(0).unwrap());
        assert_eq!(before, after, "unshared section mutates in place");
        assert_eq!(c.get_block(0, -64, 0), 1);
        assert_eq!(c.get_block(1, -64, 1), 2);
    }

    #[test]
    fn editing_one_section_leaves_siblings_untouched() {
        // A block update must not rebuild — or even touch — other sections.
        let mut c = modern();
        c.set_block(0, -64, 0, 1); // section 0
        c.set_block(0, 0, 0, 2); // section 4 (y=0)
        let s0 = c.section_arc(0).expect("section 0 present");
        let s4_before = Arc::as_ptr(&c.section_arc(4).unwrap());

        // Fork section 0 (snapshot held); section 4 must be undisturbed.
        c.set_block(5, -64, 5, 7);
        let s4_after = Arc::as_ptr(&c.section_arc(4).unwrap());
        assert_eq!(s4_before, s4_after, "sibling section identity preserved");
        assert_eq!(s0.get_block(0, 0, 0), 1, "snapshot of section 0 isolated");
    }
}
