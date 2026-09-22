//! A dense, palette-indexed block field over a fixed axis-aligned box — O(1)
//! array access instead of a coordinate-keyed block-state map.
//!
//! # Why this exists
//!
//! Composing carvers over `CarveGrid` (`crate::carver::CarveGrid`), itself
//! built from coordinate-keyed shapes designed for parity harnesses (a
//! fixture is naturally sparse/keyed data), turned into a
//! measured regression once the same shape carried the *production*
//! per-chunk composition path: a 144-chunk sweep went from sub-second to
//! ~68s in debug. Every carve read/write and every materialisation cell pays a
//! hash of a 3-tuple key plus, for a write, a fresh heap allocation — for a
//! `16×384×16` chunk that is ~98,304 cells, and ore composition (which runs the
//! pre-ore pipeline, carve included, for all 9 chunks in its 3×3
//! neighbourhood) multiplies that by 9 again.
//!
//! [`DenseBlockGrid`] is the fix: a flat `Vec<u16>` addressed by simple
//! arithmetic, palette-interned exactly like
//! [`crate::overworld::GeneratedColumn`] already is — this is *the* dense
//! representation the engine converges on at the end of every chunk's
//! pipeline anyway, so building the working grid this way from the start
//! means [`crate::overworld::OverworldGenerator::intern_from_dense`] can
//! adopt a centre-chunk-sized grid's palette/blocks directly instead of
//! re-hashing every cell a second time.
//!
#[cfg(test)]
use std::collections::HashMap;
use std::sync::Arc;

use lodestone_worldgen_core::hash::FastMap;
use lodestone_data::block_states::{self, StateId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseStateFacts {
    Builtin {
        is_air: bool,
        is_fluid: bool,
        blocks_motion: bool,
    },
}

impl BaseStateFacts {
    pub const fn air() -> Self {
        Self::Builtin {
            is_air: true,
            is_fluid: false,
            blocks_motion: false,
        }
    }

    #[inline]
    pub const fn is_ocean_floor(self) -> bool {
        match self {
            Self::Builtin { blocks_motion, .. } => blocks_motion,
        }
    }

    #[inline]
    pub const fn is_motion_blocking(self) -> bool {
        match self {
            Self::Builtin { is_air, is_fluid, blocks_motion } => {
                !is_air && (is_fluid || blocks_motion)
            }
        }
    }
}

#[inline]
pub(crate) fn air_state() -> StateId {
    block_states::air_state()
}

#[inline]
fn base_state(state: StateId) -> StateId {
    state.block().default_state()
}

#[inline]
pub(crate) fn base_facts(state: StateId) -> BaseStateFacts {
    let block = state.block();
    BaseStateFacts::Builtin {
        is_air: matches!(
            block,
            lodestone_data::block::Block::Air
                | lodestone_data::block::Block::CaveAir
                | lodestone_data::block::Block::VoidAir
        ),
        is_fluid: lodestone_data::snow_support::has_fluid_state(state),
        blocks_motion: lodestone_data::block_solidity::blocks_motion(state),
    }
}
use crate::structure::StructureMutationSink;

/// A dense block field over `[min_x, min_x+size_x) × [min_y, min_y+size_y) ×
/// [min_z, min_z+size_z)`, palette-indexed the same way
/// [`crate::overworld::GeneratedColumn`] is. A read outside the box returns
/// [`StateId::AIR`]; a write outside the box is a no-op.
///
/// # The local palette holds ids, and its *order* is unchanged
///
/// The palette contains canonical [`StateId`]s and preserves first-write order.
#[derive(Debug, Clone)]
pub struct DenseBlockGrid {
    min_x: i32,
    min_y: i32,
    min_z: i32,
    size_x: i32,
    size_y: i32,
    size_z: i32,
    /// Canonical palette, in first-write order (see the type doc).
    palette: Vec<StateId>,
    /// The base-state id for each palette entry.
    palette_bases: Vec<StateId>,
    /// Typed physical facts for each palette entry's canonical state.
    palette_base_facts: Vec<BaseStateFacts>,
    /// Reverse lookup for [`Self::palette`] — **not** an ordered structure, and
    /// never iterated (see U17's note on [`FastMap`]). `palette` is the thing
    /// whose order reaches the wire, and it is a `Vec` appended in first-write
    /// order; `index_of` only answers "is this state already in it".
    ///
    /// [`FastMap`] rather than the default hasher because this is probed on
    /// **every block write** — ~98,304 per chunk fill, ×25 for a cold column's
    /// pre-ore closure — on a `u16` key. U17's profile measured that probe at
    /// 11.8% of all SipHash time in the pipeline.
    index_of: FastMap<StateId, u16>,
    /// Cell indices are shared by cheap read snapshots and detached on the
    /// first write. Decoration keeps the cached terrain immutable while
    /// returning a modified column, so this avoids eagerly copying the whole
    /// 16x128x16 carrier when a source snapshot is installed.
    blocks: Arc<Vec<u16>>,
}

fn palette_index(
    palette: &mut Vec<StateId>,
    palette_bases: &mut Vec<StateId>,
    palette_base_facts: &mut Vec<BaseStateFacts>,
    index_of: &mut FastMap<StateId, u16>,
    state: StateId,
) -> u16 {
    if let Some(&id) = index_of.get(&state) {
        crate::counters::bump_palette_intern_hit();
        id
    } else {
        crate::counters::bump_palette_intern_new();
        let id = u16::try_from(palette.len()).expect("more than 65,536 palette entries in one grid");
        palette.push(state);
        let base = base_state(state);
        palette_bases.push(base);
        palette_base_facts.push(base_facts(state));
        index_of.insert(state, id);
        id
    }
}

#[inline]
fn direct_palette_index(
    palette: &mut Vec<StateId>,
    palette_bases: &mut Vec<StateId>,
    palette_base_facts: &mut Vec<BaseStateFacts>,
    index_of: &mut FastMap<StateId, u16>,
    direct_index: &mut [u16],
    state: StateId,
) -> u16 {
    let slot = direct_index
        .get_mut(state.index())
        .expect("generated state id outside the direct palette index");
    if *slot != u16::MAX {
        crate::counters::bump_palette_intern_hit();
        return *slot;
    }
    crate::counters::bump_palette_intern_new();
    let id = u16::try_from(palette.len()).expect("more than 65,536 palette entries in one grid");
    palette.push(state);
    let base = base_state(state);
    palette_bases.push(base);
    palette_base_facts.push(base_facts(state));
    index_of.insert(state, id);
    *slot = id;
    id
}

impl DenseBlockGrid {
    /// Explicit debug/test constructor that resolves one named default.
    #[cfg(test)]
    #[must_use]
    #[doc(hidden)]
    pub fn new_named(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: &str,
    ) -> Self {
        let default = StateId::from_state_str(default).expect("test state is in generated table");
        Self::with_default(min_x, min_y, min_z, size_x, size_y, size_z, default)
    }

    /// A grid over the given box, every cell initialised to `default` (palette
    /// index 0).
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn with_default(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: StateId,
    ) -> Self {
        let cells = (size_x.max(0) as usize) * (size_y.max(0) as usize) * (size_z.max(0) as usize);
        Self::with_default_and_blocks(
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            default,
            vec![0u16; cells],
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_default_and_blocks(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: StateId,
        blocks: Vec<u16>,
    ) -> Self {
        // Production terrain grids normally carry a few dozen distinct states.
        // Reserve that small palette once instead of growing the three palette
        // vectors and reverse map in lock-step as surface/carver writes arrive.
        // This changes capacities only: insertion order and the emitted palette
        // remain exactly the same, while cold dependency closures avoid repeated
        // metadata reallocations. The cell buffer below is still sized exactly
        // to the requested box.
        const INITIAL_PALETTE_CAPACITY: usize = 32;
        let mut index_of = FastMap::with_capacity_and_hasher(
            INITIAL_PALETTE_CAPACITY,
            Default::default(),
        );
        index_of.insert(default, 0u16);
        let cells = (size_x.max(0) as usize) * (size_y.max(0) as usize) * (size_z.max(0) as usize);
        assert_eq!(blocks.len(), cells, "dense grid carrier length must match bounds");
        let default_base = base_state(default);
        let mut palette = Vec::with_capacity(INITIAL_PALETTE_CAPACITY);
        palette.push(default);
        let mut palette_bases = Vec::with_capacity(INITIAL_PALETTE_CAPACITY);
        palette_bases.push(default_base);
        let mut palette_base_facts = Vec::with_capacity(INITIAL_PALETTE_CAPACITY);
        palette_base_facts.push(base_facts(default));
        Self {
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            palette,
            palette_bases,
            palette_base_facts,
            index_of,
            blocks: Arc::new(blocks),
        }
    }

    /// Builds a grid while invoking `state_at` in the palette's observable
    /// first-write order: z, x, then y. The cell carrier is detached once and
    /// filled by index, avoiding the copy-on-write check in every write.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn from_ordered_state_fn(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: StateId,
        mut state_at: impl FnMut(i32, i32, i32) -> StateId,
    ) -> Self {
        assert!(size_x >= 0 && size_y >= 0 && size_z >= 0, "grid size is negative");
        let mut grid = Self::with_default(
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            default,
        );
        let blocks = Arc::get_mut(&mut grid.blocks).expect("new grid carrier must be uniquely owned");
        let mut direct_index = vec![u16::MAX; block_states::STATE_COUNT as usize];
        direct_index[default.index()] = 0;
        let (palette, palette_bases, palette_base_facts, index_of) = (
            &mut grid.palette,
            &mut grid.palette_bases,
            &mut grid.palette_base_facts,
            &mut grid.index_of,
        );
        for lz in 0..size_z {
            for lx in 0..size_x {
                for ly in 0..size_y {
                    let state = state_at(min_x + lx, min_y + ly, min_z + lz);
                    let id = direct_palette_index(
                        palette,
                        palette_bases,
                        palette_base_facts,
                        index_of,
                        &mut direct_index,
                        state,
                    );
                    let index = ((ly * size_z + lz) * size_x + lx) as usize;
                    blocks[index] = id;
                }
            }
        }
        let cells = (size_x as u64) * (size_y as u64) * (size_z as u64);
        crate::counters::bump_logical_write(
            crate::counters::MemoryBoundary::BlockGrid,
            cells,
            cells * std::mem::size_of::<u16>() as u64,
        );
        grid
    }

    /// Builds a grid by rewriting an already packed carrier in the palette's
    /// observable first-write order (z, x, y). `state_at` receives the packed
    /// source value before that cell is replaced by its local palette index.
    /// This is the allocation-free handoff for producers whose source carrier
    /// is already `u16`-wide.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn from_ordered_packed_state_fn(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: StateId,
        blocks: Vec<u16>,
        mut state_at: impl FnMut(i32, i32, i32, usize, u16) -> StateId,
    ) -> Self {
        assert!(size_x >= 0 && size_y >= 0 && size_z >= 0, "grid size is negative");
        let mut grid = Self::with_default_and_blocks(
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            default,
            blocks,
        );
        let blocks = Arc::get_mut(&mut grid.blocks).expect("new grid carrier must be uniquely owned");
        let mut direct_index = vec![u16::MAX; block_states::STATE_COUNT as usize];
        direct_index[default.index()] = 0;
        let (palette, palette_bases, palette_base_facts, index_of) = (
            &mut grid.palette,
            &mut grid.palette_bases,
            &mut grid.palette_base_facts,
            &mut grid.index_of,
        );
        for lz in 0..size_z {
            for lx in 0..size_x {
                for ly in 0..size_y {
                    let index = ((ly * size_z + lz) * size_x + lx) as usize;
                    let source = blocks[index];
                    let state = state_at(min_x + lx, min_y + ly, min_z + lz, index, source);
                    blocks[index] = direct_palette_index(
                        palette,
                        palette_bases,
                        palette_base_facts,
                        index_of,
                        &mut direct_index,
                        state,
                    );
                }
            }
        }
        let cells = (size_x as u64) * (size_y as u64) * (size_z as u64);
        crate::counters::bump_logical_write(
            crate::counters::MemoryBoundary::BlockGrid,
            cells,
            cells * std::mem::size_of::<u16>() as u64,
        );
        grid
    }

    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let lx = x - self.min_x;
        let ly = y - self.min_y;
        let lz = z - self.min_z;
        if (0..self.size_x).contains(&lx) && (0..self.size_y).contains(&ly) && (0..self.size_z).contains(&lz) {
            Some(((ly * self.size_z + lz) * self.size_x + lx) as usize)
        } else {
            None
        }
    }

    /// Canonical state id at `(x, y, z)`. Air outside the box.
    #[must_use]
    pub fn get_id(&self, x: i32, y: i32, z: i32) -> StateId {
        crate::counters::bump_logical_read(crate::counters::MemoryBoundary::BlockGrid, 1, 2);
        match self.index(x, y, z) {
            Some(i) => self.palette[self.blocks[i] as usize],
            None => air_state(),
        }
    }

    /// Base-state id at `(x, y, z)`, with air outside the box.
    /// This is the numeric counterpart of stripping a state string at `'['`.
    #[must_use]
    pub fn get_base_id(&self, x: i32, y: i32, z: i32) -> StateId {
        crate::counters::bump_logical_read(crate::counters::MemoryBoundary::BlockGrid, 1, 2);
        match self.index(x, y, z) {
            Some(i) => self.palette_bases[self.blocks[i] as usize],
            None => air_state(),
        }
    }

    /// Typed physical facts for the canonical state at `(x, y, z)`.
    #[must_use]
    pub fn get_base_facts(&self, x: i32, y: i32, z: i32) -> BaseStateFacts {
        crate::counters::bump_logical_read(crate::counters::MemoryBoundary::BlockGrid, 1, 2);
        match self.index(x, y, z) {
            Some(i) => self.palette_base_facts[self.blocks[i] as usize],
            None => BaseStateFacts::air(),
        }
    }

    #[inline]
    pub(crate) fn base_facts_untracked(&self, x: i32, y: i32, z: i32) -> BaseStateFacts {
        match self.index(x, y, z) {
            Some(i) => self.palette_base_facts[self.blocks[i] as usize],
            None => BaseStateFacts::air(),
        }
    }

    /// Explicit debug/test name-resolution boundary.
    #[cfg(test)]
    #[must_use]
    #[doc(hidden)]
    pub fn get_named(&self, x: i32, y: i32, z: i32) -> String {
        crate::counters::bump_logical_read(crate::counters::MemoryBoundary::BlockGrid, 1, 2);
        self.get_id(x, y, z).canonical_state()
    }

    /// /// Writes the canonical `state` at `(x, y, z)`. A no-op outside the box
    /// (matching the prior `HashMap`-keyed grids' implicit contract: nothing in
    /// this engine writes outside the box it built a working grid for).
    ///
    /// The zero-allocation write path: a new palette entry costs a `Vec` push
    /// and a `u16`-keyed map insert, and **allocates nothing** — this is where
    /// U3's acceptance criterion is paid.
    pub fn set_id(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        let Some(i) = self.index(x, y, z) else {
            return;
        };
        self.set_id_at_index(i, state);
    }

    pub fn set_id_observed(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        state: StateId,
        source: (i32, i32),
        step: i32,
        sink: &mut dyn StructureMutationSink,
    ) {
        let Some(i) = self.index(x, y, z) else {
            return;
        };
        sink.record_structure_mutation(source, step, [x, y, z], state);
        self.set_id_at_index(i, state);
    }

    fn set_id_at_index(&mut self, i: usize, state: StateId) {
        let id = self.palette_index(state);
        Arc::make_mut(&mut self.blocks)[i] = id;
        crate::counters::bump_logical_write(crate::counters::MemoryBoundary::BlockGrid, 1, 2);
    }

    fn palette_index(&mut self, state: StateId) -> u16 {
        palette_index(
            &mut self.palette,
            &mut self.palette_bases,
            &mut self.palette_base_facts,
            &mut self.index_of,
            state,
        )
    }

    /// Explicit debug/test name-resolution boundary.
    #[cfg(test)]
    #[doc(hidden)]
    pub fn set_named(&mut self, x: i32, y: i32, z: i32, state: &str) {
        let id = StateId::from_state_str(state).expect("test state is in generated table");
        self.set_id(x, y, z, id);
    }

    /// Copies an axis-aligned box from another grid. The observable result is
    /// identical to `get_id`/`set_id` in
    /// y-z-x order: destination palette entries are therefore still appended
    /// in first-write order. Each x row uses direct slice indexing instead of
    /// paying coordinate bounds checks and source palette resolution for every
    /// cell. A lazy source-to-destination palette mapping also avoids hashing
    /// the same small set of state ids once per copied cell.
    ///
    /// # Panics
    ///
    /// Panics when either box lies outside its grid.
    #[allow(clippy::too_many_arguments)]
    pub fn copy_box_from(
        &mut self,
        source: &Self,
        source_x: i32,
        source_y: i32,
        source_z: i32,
        destination_x: i32,
        destination_y: i32,
        destination_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
    ) {
        assert!(size_x >= 0 && size_y >= 0 && size_z >= 0, "copy size is negative");
        if size_x == 0 || size_y == 0 || size_z == 0 {
            return;
        }
        assert!(
            source.index(source_x, source_y, source_z).is_some()
                && source.index(source_x + size_x - 1, source_y + size_y - 1, source_z + size_z - 1).is_some(),
            "source copy box is outside the grid",
        );
        assert!(
            self.index(destination_x, destination_y, destination_z).is_some()
                && self.index(destination_x + size_x - 1, destination_y + size_y - 1, destination_z + size_z - 1).is_some(),
            "destination copy box is outside the grid",
        );

        let width = size_x as usize;
        let copied_cells = (size_x as u64) * (size_y as u64) * (size_z as u64);
        crate::counters::bump_logical_read(
            crate::counters::MemoryBoundary::BlockGrid,
            copied_cells,
            copied_cells * 2,
        );
        crate::counters::bump_logical_write(
            crate::counters::MemoryBoundary::BlockGrid,
            copied_cells,
            copied_cells * 2,
        );
        // Keep this mapping lazy. Eagerly mapping `source.palette` would alter
        // the destination palette's first-write order (which is observable in
        // the packet), while laziness preserves the exact scan-order contract.
        // `u16::MAX` is not a valid local palette index: the insertion path
        // checks the destination palette length before assigning an id.
        const INLINE_PALETTE_MAPPING: usize = 256;
        let mut inline_source_to_destination = [u16::MAX; INLINE_PALETTE_MAPPING];
        let mut heap_source_to_destination = (source.palette.len() > INLINE_PALETTE_MAPPING)
            .then(|| vec![u16::MAX; source.palette.len()]);
        let destination_offset_x = destination_x - self.min_x;
        let destination_offset_y = destination_y - self.min_y;
        let destination_offset_z = destination_z - self.min_z;
        let destination_size_x = self.size_x;
        let destination_size_z = self.size_z;
        // Detach the destination carrier once for the whole transfer. Calling
        // `Arc::make_mut` for every row repeats the refcount/uniqueness check
        // and needlessly re-enters the copy-on-write boundary in this hot
        // path; the palette still mutates in the same y-z-x scan order below.
        let destination_blocks = Arc::make_mut(&mut self.blocks);
        for y in 0..size_y {
            for z in 0..size_z {
                let source_start = source
                    .index(source_x, source_y + y, source_z + z)
                    .expect("validated source row");
                let destination_start = (((destination_offset_y + y) * destination_size_z
                    + destination_offset_z
                    + z)
                    * destination_size_x
                    + destination_offset_x) as usize;
                let source_row = &source.blocks[source_start..source_start + width];
                let destination_row = &mut destination_blocks
                    [destination_start..destination_start + width];
                for (destination, &source_index) in destination_row.iter_mut().zip(source_row) {
                    let source_index = source_index as usize;
                    let destination_index = if source.palette.len() <= INLINE_PALETTE_MAPPING {
                        &mut inline_source_to_destination[source_index]
                    } else {
                        &mut heap_source_to_destination
                            .as_mut()
                            .expect("large source palette mapping")[source_index]
                    };
                    if *destination_index == u16::MAX {
                        let state = source.palette[source_index];
                        let id = if let Some(&id) = self.index_of.get(&state) {
                            id
                        } else {
                            let id = u16::try_from(self.palette.len())
                                .expect("more than 65,536 palette entries in one grid");
                            self.palette.push(state);
                            let base = base_state(state);
                            self.palette_bases.push(base);
                            self.palette_base_facts.push(base_facts(state));
                            self.index_of.insert(state, id);
                            id
                        };
                        *destination_index = id;
                    }
                    *destination = *destination_index;
                }
            }
        }
    }

    /// Builds a chunk-sized grid from canonical global state ids.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn from_canonical_states(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        mut state_at: impl FnMut(i32, i32, i32) -> lodestone_data::block_states::StateId,
    ) -> Self {
        let mut grid = Self::with_default(
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            air_state(),
        );
        for y in min_y..min_y + size_y {
            for z in min_z..min_z + size_z {
                for x in min_x..min_x + size_x {
                    let state = state_at(x, y, z);
                    if state != air_state() {
                        grid.set_id(x, y, z, state);
                    }
                }
            }
        }
        grid
    }

    /// Explicit debug/test constructor from named state fixtures.
    #[cfg(test)]
    #[must_use]
    #[doc(hidden)]
    pub fn from_named_hashmap(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        map: &HashMap<(i32, i32, i32), String>,
    ) -> Self {
        let mut grid = Self::new_named(min_x, min_y, min_z, size_x, size_y, size_z, "minecraft:air");
        for (&(x, y, z), state) in map {
            grid.set_named(x, y, z, state);
        }
        grid
    }

    /// Explicit debug/test destructor resolving every cell to a named state.
    #[cfg(test)]
    #[must_use]
    #[doc(hidden)]
    pub fn into_named_hashmap(self) -> HashMap<(i32, i32, i32), String> {
        let mut out = HashMap::with_capacity(self.blocks.len());
        for ly in 0..self.size_y {
            for lz in 0..self.size_z {
                for lx in 0..self.size_x {
                    let i = ((ly * self.size_z + lz) * self.size_x + lx) as usize;
                    let state = self.palette[self.blocks[i] as usize].canonical_state();
                    out.insert((self.min_x + lx, self.min_y + ly, self.min_z + lz), state);
                }
            }
        }
        out
    }

    /// The box's origin and size, for a caller that needs to re-derive
    /// `(lx, ly, lz)` bounds (e.g. [`crate::overworld::GeneratedColumn`]
    /// adoption).
    #[must_use]
    pub fn bounds(&self) -> (i32, i32, i32, i32, i32, i32) {
        (self.min_x, self.min_y, self.min_z, self.size_x, self.size_y, self.size_z)
    }

    /// Explicit debug/test export resolving the local palette to names.
    #[cfg(test)]
    #[must_use]
    #[doc(hidden)]
    pub fn into_named_palette_and_blocks(self) -> (Vec<String>, Vec<u16>) {
        crate::counters::bump_logical_read(
            crate::counters::MemoryBoundary::BlockGrid,
            self.blocks.len() as u64,
            (self.blocks.len() * std::mem::size_of::<u16>()) as u64,
        );
        crate::counters::bump_full_column_conversion(self.blocks.len() as u64);
        let palette = self
            .palette
            .iter()
            .map(|state| state.canonical_state())
            .collect();
        (palette, Arc::unwrap_or_clone(self.blocks))
    }

    /// Consumes the grid and emits one axis-aligned box in the same palette
    /// order as copying that box into a fresh grid initialized with `default`.
    ///
    /// This is the output boundary used by a decorated region: the working
    /// grid is a three-by-three halo, but only its centre chunk is served. A
    /// temporary centre-sized grid would allocate and copy the whole output
    /// once more; folding the box directly keeps the observable first-write
    /// palette order while avoiding that carrier allocation.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn into_named_palette_and_blocks_box(
        self,
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: StateId,
    ) -> (Vec<String>, Vec<u16>) {
        let converted_cells = (size_x.max(0) as u64)
            * (size_y.max(0) as u64)
            * (size_z.max(0) as u64);
        crate::counters::bump_logical_read(
            crate::counters::MemoryBoundary::BlockGrid,
            converted_cells,
            converted_cells * 2,
        );
        crate::counters::bump_full_column_conversion(
            converted_cells,
        );
        assert!(size_x >= 0 && size_y >= 0 && size_z >= 0, "box size is negative");
        if size_x == 0 || size_y == 0 || size_z == 0 {
            return (vec![default.canonical_state()], Vec::new());
        }
        let source_index = |x: i32, y: i32, z: i32| {
            self.index(x, y, z).expect("output box is outside the grid")
        };
        assert!(
            self.index(min_x, min_y, min_z).is_some()
                && self
                    .index(min_x + size_x - 1, min_y + size_y - 1, min_z + size_z - 1)
                    .is_some(),
            "output box is outside the grid",
        );

        let cell_count = (size_x as usize) * (size_y as usize) * (size_z as usize);
        let capacity = self.palette.len().min(cell_count + 1);
        let mut palette = Vec::with_capacity(capacity);
        let mut index_of = FastMap::with_capacity_and_hasher(capacity.max(1), Default::default());
        palette.push(default);
        index_of.insert(default, 0);

        let mut blocks = Vec::with_capacity(cell_count);
        for y in min_y..min_y + size_y {
            for z in min_z..min_z + size_z {
                for x in min_x..min_x + size_x {
                    let source_state = self.palette[self.blocks[source_index(x, y, z)] as usize];
                    let local = if let Some(&local) = index_of.get(&source_state) {
                        local
                    } else {
                        let local = u16::try_from(palette.len())
                            .expect("more than 65,536 palette entries in one output box");
                        palette.push(source_state);
                        index_of.insert(source_state, local);
                        local
                    };
                    blocks.push(local);
                }
            }
        }
        (
            palette
                .into_iter()
                .map(|state| state.canonical_state())
                .collect(),
            blocks,
        )
    }

    /// The palette as interned ids, in first-write order — the allocation-free
    /// counterpart of [`Self::into_named_palette_and_blocks`], for a caller that can
    /// carry ids instead of strings.
    #[must_use]
    pub fn into_id_palette_and_blocks(self) -> (Vec<StateId>, Vec<u16>) {
        let (palette, blocks) = self.into_id_palette_and_shared_blocks();
        let blocks = Arc::unwrap_or_clone(blocks);
        crate::counters::bump_full_column_conversion(blocks.len() as u64);
        (palette, blocks)
    }

    /// The palette and shared dense cell carrier, preserving the ownership
    /// boundary for a consumer that can defer section packing.
    #[must_use]
    pub fn into_id_palette_and_shared_blocks(self) -> (Vec<StateId>, Arc<Vec<u16>>) {
        crate::counters::bump_logical_read(
            crate::counters::MemoryBoundary::BlockGrid,
            self.blocks.len() as u64,
            (self.blocks.len() * std::mem::size_of::<u16>()) as u64,
        );
        (self.palette, self.blocks)
    }

    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn into_id_palette_and_blocks_box(
        self,
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: StateId,
    ) -> (Vec<StateId>, Vec<u16>) {
        let converted_cells = (size_x.max(0) as u64)
            * (size_y.max(0) as u64)
            * (size_z.max(0) as u64);
        crate::counters::bump_logical_read(
            crate::counters::MemoryBoundary::BlockGrid,
            converted_cells,
            converted_cells * std::mem::size_of::<u16>() as u64,
        );
        crate::counters::bump_full_column_conversion(converted_cells);
        assert!(size_x >= 0 && size_y >= 0 && size_z >= 0, "box size is negative");
        if size_x == 0 || size_y == 0 || size_z == 0 {
            return (vec![default], Vec::new());
        }
        let source_index = |x: i32, y: i32, z: i32| {
            self.index(x, y, z).expect("output box is outside the grid")
        };
        assert!(
            self.index(min_x, min_y, min_z).is_some()
                && self.index(min_x + size_x - 1, min_y + size_y - 1, min_z + size_z - 1).is_some(),
            "output box is outside the grid",
        );
        let cell_count = (size_x as usize) * (size_y as usize) * (size_z as usize);
        let capacity = self.palette.len().min(cell_count + 1);
        let mut palette = Vec::with_capacity(capacity);
        let mut index_of = FastMap::with_capacity_and_hasher(capacity.max(1), Default::default());
        palette.push(default);
        index_of.insert(default, 0);
        let mut blocks = Vec::with_capacity(cell_count);
        for y in min_y..min_y + size_y {
            for z in min_z..min_z + size_z {
                for x in min_x..min_x + size_x {
                    let state = self.palette[self.blocks[source_index(x, y, z)] as usize];
                    let local = if let Some(&local) = index_of.get(&state) {
                        local
                    } else {
                        let local = u16::try_from(palette.len())
                            .expect("more than 65,536 palette entries in one output box");
                        palette.push(state);
                        index_of.insert(state, local);
                        local
                    };
                    blocks.push(local);
                }
            }
        }
        (palette, blocks)
    }
}

#[cfg(test)]
impl DenseBlockGrid {
    #[must_use]
    pub fn new(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: &str,
    ) -> Self {
        Self::new_named(min_x, min_y, min_z, size_x, size_y, size_z, default)
    }

    #[must_use]
    pub fn get(&self, x: i32, y: i32, z: i32) -> String {
        self.get_named(x, y, z)
    }

    pub fn set(&mut self, x: i32, y: i32, z: i32, state: &str) {
        self.set_named(x, y, z, state);
    }

    #[must_use]
    pub fn from_hashmap(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        map: &HashMap<(i32, i32, i32), String>,
    ) -> Self {
        Self::from_named_hashmap(min_x, min_y, min_z, size_x, size_y, size_z, map)
    }

    #[must_use]
    pub fn into_hashmap(self) -> HashMap<(i32, i32, i32), String> {
        self.into_named_hashmap()
    }

    #[must_use]
    pub fn into_palette_and_blocks(self) -> (Vec<String>, Vec<u16>) {
        self.into_named_palette_and_blocks()
    }

    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn into_palette_and_blocks_box(
        self,
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: StateId,
    ) -> (Vec<String>, Vec<u16>) {
        self.into_named_palette_and_blocks_box(
            min_x, min_y, min_z, size_x, size_y, size_z, default,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(spec: &str) -> StateId {
        StateId::from_state_str(spec).expect("test state is in the generated table")
    }

    fn packed_result_digest(palette: &[String], blocks: &[u16]) -> u64 {
        let mut digest = 0xcbf29ce484222325u64;
        for name in palette {
            for byte in name.as_bytes() {
                digest = (digest ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
            digest = (digest ^ 0xff).wrapping_mul(0x100000001b3);
        }
        for block in blocks {
            for byte in block.to_le_bytes() {
                digest = (digest ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        }
        digest
    }

    #[test]
    fn packed_ordered_handoff_matches_allocating_reference_and_control() {
        let air = state("minecraft:air");
        let stone = state("minecraft:stone");
        let water = state("minecraft:water");
        let lava = state("minecraft:lava");
        let packed: Vec<u16> = (0..(4 * 3 * 2))
            .map(|index| match index % 7 {
                0 | 4 => 1,
                1 | 5 => 2,
                2 => 3,
                _ => 0,
            })
            .collect();
        let state_for_code = |code| match code {
            0 => air,
            1 => stone,
            2 => water,
            3 => lava,
            _ => panic!("invalid packed test code"),
        };
        let reference = DenseBlockGrid::from_ordered_state_fn(
            0,
            0,
            0,
            4,
            3,
            2,
            air,
            |x, y, z| state_for_code(packed[((y * 2 + z) * 4 + x) as usize]),
        );
        let actual = DenseBlockGrid::from_ordered_packed_state_fn(
            0,
            0,
            0,
            4,
            3,
            2,
            air,
            packed.clone(),
            |x, y, z, _index, code| {
                assert_eq!(code, packed[((y * 2 + z) * 4 + x) as usize]);
                state_for_code(code)
            },
        );
        let expected = reference.into_named_palette_and_blocks();
        let result = actual.into_named_palette_and_blocks();
        assert_eq!(result, expected, "packed handoff changed palette or blocks");
        let expected_palette = vec![
            "minecraft:air".to_owned(),
            "minecraft:stone".to_owned(),
            "minecraft:water".to_owned(),
            "minecraft:lava".to_owned(),
        ];
        let expected_blocks: Vec<u16> = packed
            .iter()
            .map(|&code| match code {
                0 => 0,
                1 => 1,
                2 => 2,
                3 => 3,
                _ => panic!("invalid packed test code"),
            })
            .collect();
        assert_eq!(result.0, expected_palette, "packed palette order changed");
        assert_eq!(result.1, expected_blocks, "packed block indices changed");
        let digest = packed_result_digest(&result.0, &result.1);
        assert_eq!(digest, 0xabd0_10dc_d4dd_58e6, "packed output digest changed");

        let mut control = packed;
        control[0] = 3;
        let control_air = state("minecraft:air");
        let control_stone = state("minecraft:stone");
        let control_water = state("minecraft:water");
        let control_lava = state("minecraft:lava");
        let changed = DenseBlockGrid::from_ordered_packed_state_fn(
            0,
            0,
            0,
            4,
            3,
            2,
            control_air,
            control,
            |_x, _y, _z, _index, code| match code {
                0 => control_air,
                1 => control_stone,
                2 => control_water,
                3 => control_lava,
                _ => panic!("invalid packed test code"),
            },
        );
        let changed_result = changed.into_named_palette_and_blocks();
        assert_ne!(
            digest,
            packed_result_digest(&changed_result.0, &changed_result.1),
            "changed packed input must affect the output digest"
        );
    }

    #[test]
    fn get_set_round_trips_within_bounds() {
        let mut g = DenseBlockGrid::new_named(5, -64, 5, 4, 8, 4, "minecraft:air");
        assert_eq!(g.get_named(5, -64, 5), "minecraft:air");
        g.set_named(6, -60, 7, "minecraft:stone");
        assert_eq!(g.get_named(6, -60, 7), "minecraft:stone");
        // Neighbouring cells are untouched.
        assert_eq!(g.get_named(6, -60, 6), "minecraft:air");
    }

    #[test]
    fn bulk_copy_preserves_cells_and_first_write_palette_order() {
        let air = state("minecraft:air");
        let mut source = DenseBlockGrid::with_default(16, 0, 32, 4, 3, 4, air);
        source.set_named(17, 1, 33, "minecraft:stone");
        source.set_named(18, 1, 33, "minecraft:dirt");
        let mut destination = DenseBlockGrid::with_default(0, 0, 0, 8, 3, 8, air);

        destination.copy_box_from(&source, 16, 0, 32, 2, 0, 2, 4, 3, 4);

        assert_eq!(destination.get_named(3, 1, 3), "minecraft:stone");
        assert_eq!(destination.get_named(4, 1, 3), "minecraft:dirt");
        assert_eq!(destination.get_named(0, 0, 0), "minecraft:air");
        let (palette, _) = destination.into_named_palette_and_blocks();
        assert_eq!(palette, ["minecraft:air", "minecraft:stone", "minecraft:dirt"]);
    }

    #[test]
    fn bulk_copy_detaches_shared_destination_without_changing_alias() {
        let air = state("minecraft:air");
        let mut source = DenseBlockGrid::with_default(0, 0, 0, 2, 1, 1, air);
        source.set_named(0, 0, 0, "minecraft:stone");
        source.set_named(1, 0, 0, "minecraft:dirt");

        let mut destination = DenseBlockGrid::with_default(8, 0, 8, 2, 1, 1, air);
        let alias = destination.clone();
        destination.copy_box_from(&source, 0, 0, 0, 8, 0, 8, 2, 1, 1);

        assert_eq!(destination.get_named(8, 0, 8), "minecraft:stone");
        assert_eq!(destination.get_named(9, 0, 8), "minecraft:dirt");
        assert_eq!(alias.get_named(8, 0, 8), "minecraft:air");
        assert_eq!(alias.get_named(9, 0, 8), "minecraft:air");
    }

    #[test]
    fn boxed_output_matches_copy_into_a_centre_grid() {
        let air = state("minecraft:air");
        let mut source = DenseBlockGrid::with_default(-16, 0, -16, 48, 4, 48, air);
        source.set_named(-1, 1, -1, "minecraft:stone");
        source.set_named(0, 2, 0, "minecraft:dirt");

        let mut copied = DenseBlockGrid::with_default(0, 0, 0, 16, 4, 16, air);
        copied.copy_box_from(&source, 0, 0, 0, 0, 0, 0, 16, 4, 16);
        let expected = copied.into_named_palette_and_blocks();
        let actual = source.into_named_palette_and_blocks_box(0, 0, 0, 16, 4, 16, air);
        assert_eq!(actual, expected);
    }

    #[test]
    fn out_of_bounds_read_is_air_and_write_is_noop() {
        let mut g = DenseBlockGrid::new_named(0, 0, 0, 2, 2, 2, "minecraft:air");
        assert_eq!(g.get_named(100, 100, 100), "minecraft:air");
        g.set_named(100, 100, 100, "minecraft:stone"); // must not panic
        assert_eq!(g.get_named(100, 100, 100), "minecraft:air");
    }

    #[test]
    fn hashmap_round_trip_preserves_every_cell() {
        let mut map = HashMap::new();
        let states = [
            "minecraft:stone",
            "minecraft:dirt",
            "minecraft:granite",
            "minecraft:andesite",
            "minecraft:calcite",
        ];
        for x in 0_i32..3 {
            for y in 0_i32..3 {
                for z in 0_i32..3 {
                    map.insert((x, y, z), states[((x + y + z) as usize) % states.len()].to_owned());
                }
            }
        }
        let grid = DenseBlockGrid::from_named_hashmap(0, 0, 0, 3, 3, 3, &map);
        let back = grid.into_named_hashmap();
        assert_eq!(back, map);
    }

    #[test]
    fn palette_order_is_first_write_order() {
        // The invariant that keeps canonical id assignment order off the wire
        // (see the canonical state registry). Written deliberately in an order
        // that does *not* match either alphabetical order or global state ids.
        let mut g = DenseBlockGrid::new_named(0, 0, 0, 4, 4, 4, "minecraft:air");
        g.set_named(0, 0, 0, "minecraft:granite");
        g.set_named(1, 0, 0, "minecraft:andesite");
        g.set_named(2, 0, 0, "minecraft:granite");
        g.set_named(3, 0, 0, "minecraft:calcite");
        let (palette, _) = g.into_named_palette_and_blocks();
        assert_eq!(
            palette,
            vec![
                "minecraft:air".to_string(),
                "minecraft:granite".to_string(),
                "minecraft:andesite".to_string(),
                "minecraft:calcite".to_string(),
            ],
        );
    }

    #[test]
    fn an_id_read_from_one_grid_writes_into_another_sharing_canonical_state() {
        let air = state("minecraft:air");
        let mut src = DenseBlockGrid::with_default(0, 0, 0, 2, 2, 2, air);
        let mut dst = DenseBlockGrid::with_default(0, 0, 0, 2, 2, 2, air);
        src.set_named(1, 1, 1, "minecraft:deepslate");

        dst.set_id(0, 0, 0, src.get_id(1, 1, 1));

        assert_eq!(dst.get_named(0, 0, 0), "minecraft:deepslate");
    }

    #[test]
    fn id_palette_resolves_consistently_at_debug_boundary() {
        let mut g = DenseBlockGrid::new_named(0, 0, 0, 3, 3, 3, "minecraft:air");
        let states = [
            "minecraft:stone",
            "minecraft:deepslate",
            "minecraft:oak_log[axis=y]",
            "minecraft:stone",
            "minecraft:gravel",
        ];
        for (i, state) in states.iter().enumerate() {
            let i = i as i32;
            g.set_named(i % 3, i / 3, 0, state);
        }
        for x in 0..3 {
            for y in 0..3 {
                for z in 0..3 {
                    assert_eq!(
                        g.get_named(x, y, z),
                        g.get_id(x, y, z).canonical_state(),
                        "named debug resolution disagreed with the id palette at ({x}, {y}, {z})",
                    );
                }
            }
        }
    }

    #[test]
    fn get_id_is_air_outside_the_box() {
        let g = DenseBlockGrid::new_named(0, 0, 0, 2, 2, 2, "minecraft:air");
        assert_eq!(g.get_id(100, 100, 100), StateId::AIR);
    }

    #[test]
    fn base_id_tracks_property_states_without_string_parsing() {
        let mut g = DenseBlockGrid::new_named(0, 0, 0, 2, 1, 1, "minecraft:air");
        let bare = state("minecraft:oak_log");
        g.set_named(0, 0, 0, "minecraft:oak_log[axis=y]");
        g.set_id(1, 0, 0, bare);

        assert_eq!(g.get_base_id(0, 0, 0), bare);
        assert_eq!(g.get_base_id(1, 0, 0), bare);
        assert_eq!(g.get_base_id(100, 100, 100), StateId::AIR);
    }

    #[test]
    fn id_palette_and_string_palette_agree_entry_for_entry() {
        let mut g = DenseBlockGrid::new_named(0, 0, 0, 2, 2, 2, "minecraft:air");
        g.set_named(0, 0, 0, "minecraft:tuff");
        g.set_named(1, 0, 0, "minecraft:calcite");
        let (ids, id_blocks) = g.clone().into_id_palette_and_blocks();
        let (names, name_blocks) = g.into_named_palette_and_blocks();
        assert_eq!(id_blocks, name_blocks, "blocks must not depend on palette form");
        let resolved: Vec<String> = ids
            .iter()
            .map(|&id| {
                id.canonical_state()
            })
            .collect();
        assert_eq!(resolved, names);
    }

    #[test]
    fn repeated_writes_of_the_same_state_reuse_one_palette_entry() {
        let mut g = DenseBlockGrid::new_named(0, 0, 0, 4, 4, 4, "minecraft:air");
        for i in 0..10 {
            g.set_named(i % 4, 0, 0, "minecraft:granite");
        }
        let (palette, _blocks) = g.into_named_palette_and_blocks();
        assert_eq!(palette, vec!["minecraft:air".to_string(), "minecraft:granite".to_string()]);
    }

    fn setter_reference(states: &[StateId], size_x: i32, size_y: i32, size_z: i32) -> DenseBlockGrid {
        let air = StateId::AIR;
        let mut grid = DenseBlockGrid::with_default(
            0,
            0,
            0,
            size_x,
            size_y,
            size_z,
            air,
        );
        let mut index = 0;
        for z in 0..size_z {
            for x in 0..size_x {
                for y in 0..size_y {
                    grid.set_id(x, y, z, states[index]);
                    index += 1;
                }
            }
        }
        grid
    }

    #[test]
    fn ordered_builder_matches_setter_for_empty_and_nonempty_diffs() {
        let stone = state("minecraft:stone");
        let dirt = state("minecraft:dirt");
        let states = [
            StateId::AIR,
            StateId::AIR,
            StateId::AIR,
            StateId::AIR,
            StateId::AIR,
            StateId::AIR,
        ];
        let expected = setter_reference(&states, 2, 3, 1);
        let actual = DenseBlockGrid::from_ordered_state_fn(
            0,
            0,
            0,
            2,
            3,
            1,
            StateId::AIR,
            |x, y, z| states[((z * 2 + x) * 3 + y) as usize],
        );
        assert_eq!(
            actual.into_id_palette_and_blocks(),
            expected.into_id_palette_and_blocks()
        );

        let states = [stone, StateId::AIR, dirt, stone, dirt, StateId::AIR];
        let expected = setter_reference(&states, 2, 3, 1);
        let actual = DenseBlockGrid::from_ordered_state_fn(
            0,
            0,
            0,
            2,
            3,
            1,
            StateId::AIR,
            |x, y, z| states[((z * 2 + x) * 3 + y) as usize],
        );
        assert_eq!(
            actual.into_id_palette_and_blocks(),
            expected.into_id_palette_and_blocks()
        );
    }

    #[test]
    fn ordered_builder_negative_control_detects_changed_write_order() {
        let stone = state("minecraft:stone");
        let dirt = state("minecraft:dirt");
        let states = [stone, StateId::AIR, dirt, stone, dirt, StateId::AIR];
        let expected = setter_reference(&states, 2, 3, 1);
        let actual = DenseBlockGrid::from_ordered_state_fn(
            0,
            0,
            0,
            2,
            3,
            1,
            StateId::AIR,
            |x, y, z| {
                states[(states.len() - 1) - ((z * 2 + x) * 3 + y) as usize]
            },
        );
        assert_ne!(actual.into_id_palette_and_blocks(), expected.into_id_palette_and_blocks());
    }

    #[test]
    fn ordered_packed_builder_preserves_palette_order() {
        let states = [
            state("minecraft:stone"),
            StateId::AIR,
            state("minecraft:dirt"),
            state("minecraft:stone"),
            state("minecraft:dirt"),
            StateId::AIR,
        ];
        let expected = DenseBlockGrid::from_ordered_state_fn(
            0,
            0,
            0,
            2,
            3,
            1,
            StateId::AIR,
            |x, y, z| states[((z * 2 + x) * 3 + y) as usize],
        );
        let packed = states.iter().map(|state| state.raw() as u16).collect();
        let actual = DenseBlockGrid::from_ordered_packed_state_fn(
            0,
            0,
            0,
            2,
            3,
            1,
            StateId::AIR,
            packed,
            |_, _, _, _, raw| StateId::from_raw(raw),
        );
        assert_eq!(
            actual.into_id_palette_and_blocks(),
            expected.into_id_palette_and_blocks()
        );
    }
}
