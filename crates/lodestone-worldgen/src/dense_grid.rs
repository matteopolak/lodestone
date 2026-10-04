//! A block field over a fixed axis-aligned box. Dense indexed and raw lanes
//! share their carrier; transient End regions share nine immutable columns.
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

use crate::generated_storage::CompactBlockStorage;

const _: () = assert!(block_states::STATE_COUNT - 1 <= u16::MAX as u32);

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
    /// never iterated. `palette` is the thing
    /// whose order reaches the wire, and it is a `Vec` appended in first-write
    /// order; `index_of` only answers "is this state already in it".
    ///
    /// [`FastMap`] rather than the default hasher because this is probed on
    /// **every block write** — ~98,304 per chunk fill, ×25 for a cold column's
    /// pre-ore closure — on a `u16` key. Profiling measured that probe at
    /// 11.8% of all SipHash time in the pipeline.
    index_of: FastMap<StateId, u16>,
    storage: GridStorage,
    raw_introductions: Vec<StateId>,
    raw_introduction_index: FastMap<StateId, ()>,
    change_capture: Option<FastMap<usize, StateId>>,
}

#[derive(Debug, Clone)]
enum GridStorage {
    Indexed(Arc<Vec<u16>>),
    Raw(Arc<Vec<u16>>),
    BorrowedRegion(Box<BorrowedRegion>),
}

#[derive(Debug, Clone)]
struct BorrowedRegion {
    /// Slots follow x-major, z-fast chunk order.
    bases: [Arc<DenseBlockGrid>; 9],
    writes: FastMap<usize, StateId>,
}

impl GridStorage {
    #[inline]
    fn dense_cells(&self) -> &Arc<Vec<u16>> {
        match self {
            Self::Indexed(cells) | Self::Raw(cells) => cells,
            Self::BorrowedRegion(_) => panic!("borrowed region requires explicit materialization"),
        }
    }

    #[inline]
    fn dense_cells_mut(&mut self) -> &mut Arc<Vec<u16>> {
        match self {
            Self::Indexed(cells) | Self::Raw(cells) => cells,
            Self::BorrowedRegion(_) => panic!("borrowed region requires explicit materialization"),
        }
    }

    #[inline]
    fn is_raw(&self) -> bool {
        matches!(self, Self::Raw(_))
    }

    #[inline]
    fn is_borrowed(&self) -> bool {
        matches!(self, Self::BorrowedRegion(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DenseBlockChange {
    pub position: (i32, i32, i32),
    pub state: StateId,
}

pub(crate) enum DenseBlockGridParts {
    Indexed {
        palette: Vec<StateId>,
        blocks: Arc<Vec<u16>>,
    },
    Raw {
        introductions: Vec<StateId>,
        states: Arc<Vec<u16>>,
    },
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
            storage: GridStorage::Indexed(Arc::new(blocks)),
            raw_introductions: Vec::new(),
            raw_introduction_index: FastMap::default(),
            change_capture: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn with_default_raw_and_blocks(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: StateId,
        blocks: Vec<u16>,
    ) -> Self {
        let cells = (size_x.max(0) as usize) * (size_y.max(0) as usize) * (size_z.max(0) as usize);
        assert_eq!(blocks.len(), cells, "dense grid carrier length must match bounds");
        let mut raw_introductions = Vec::with_capacity(32);
        raw_introductions.push(default);
        let mut raw_introduction_index = FastMap::with_capacity_and_hasher(32, Default::default());
        raw_introduction_index.insert(default, ());
        Self {
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            palette: Vec::new(),
            palette_bases: Vec::new(),
            palette_base_facts: Vec::new(),
            index_of: FastMap::default(),
            storage: GridStorage::Raw(Arc::new(blocks)),
            raw_introductions,
            raw_introduction_index,
            change_capture: None,
        }
    }

    /// Shares nine immutable 16-wide columns in x-major, z-fast order. Writes
    /// remain transient; reads above or below a source's own height return air.
    pub(crate) fn borrowed_region(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        height: i32,
        bases: [Arc<DenseBlockGrid>; 9],
    ) -> Self {
        assert!(height >= 0, "grid height is negative");
        for (slot, base) in bases.iter().enumerate() {
            assert_eq!(base.min_x, min_x + (slot / 3) as i32 * 16);
            assert_eq!(base.min_z, min_z + (slot % 3) as i32 * 16);
            assert_eq!((base.size_x, base.size_z), (16, 16));
            assert!(!base.storage.is_borrowed(), "region bases must be dense immutable columns");
        }
        let mut grid = Self::with_default_and_blocks(min_x, min_y, min_z, 0, 0, 0, air_state(), Vec::new());
        grid.size_x = 48;
        grid.size_y = height;
        grid.size_z = 48;
        grid.storage = GridStorage::BorrowedRegion(Box::new(BorrowedRegion {
            bases,
            writes: FastMap::default(),
        }));
        grid
    }

    /// Creates an Overworld state field whose only dense cell lane contains
    /// canonical state IDs. `default` is the first palette introduction.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn with_default_raw(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: StateId,
    ) -> Self {
        let cells = (size_x.max(0) as usize) * (size_y.max(0) as usize) * (size_z.max(0) as usize);
        let raw = raw_state_id(default);
        Self::with_default_raw_and_blocks(
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            default,
            vec![raw; cells],
        )
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
        let blocks = Arc::get_mut(grid.storage.dense_cells_mut()).expect("new grid carrier must be uniquely owned");
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

    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn from_ordered_state_fn_raw(
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
        let cells = (size_x as usize) * (size_y as usize) * (size_z as usize);
        let raw_default = u16::try_from(default.raw()).expect("canonical state id fits raw lane");
        let mut grid = Self::with_default_raw_and_blocks(
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            default,
            vec![raw_default; cells],
        );
        let mut introductions = std::mem::take(&mut grid.raw_introductions);
        let mut direct_order = vec![u16::MAX; block_states::STATE_COUNT as usize];
        direct_order[default.index()] = 0;
        {
            let blocks = Arc::get_mut(grid.storage.dense_cells_mut()).expect("new grid carrier must be uniquely owned");
            for lz in 0..size_z {
                for lx in 0..size_x {
                    for ly in 0..size_y {
                        let state = state_at(min_x + lx, min_y + ly, min_z + lz);
                        let order = &mut direct_order[state.index()];
                        if *order == u16::MAX {
                            *order = u16::try_from(introductions.len())
                                .expect("state introduction count fits u16");
                            introductions.push(state);
                        }
                        let index = ((ly * size_z + lz) * size_x + lx) as usize;
                        blocks[index] = raw_state_id(state);
                    }
                }
            }
        }
        for &state in &introductions {
            grid.raw_introduction_index.insert(state, ());
        }
        grid.raw_introductions = introductions;
        crate::counters::bump_logical_write(
            crate::counters::MemoryBoundary::BlockGrid,
            cells as u64,
            (cells * std::mem::size_of::<u16>()) as u64,
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
        let blocks = Arc::get_mut(grid.storage.dense_cells_mut()).expect("new grid carrier must be uniquely owned");
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

    /// Builds a raw-state lane in place, preserving the z,x,y introduction
    /// order while leaving each physical cell as its canonical `u16` ID.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn from_ordered_packed_state_fn_raw(
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
        let mut grid = Self::with_default_raw_and_blocks(
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            default,
            blocks,
        );
        let mut introductions = std::mem::take(&mut grid.raw_introductions);
        let mut direct_order = vec![u16::MAX; block_states::STATE_COUNT as usize];
        direct_order[default.index()] = 0;
        {
            let blocks = Arc::get_mut(grid.storage.dense_cells_mut()).expect("new grid carrier must be uniquely owned");
            for lz in 0..size_z {
                for lx in 0..size_x {
                    for ly in 0..size_y {
                        let index = ((ly * size_z + lz) * size_x + lx) as usize;
                        let state = state_at(min_x + lx, min_y + ly, min_z + lz, index, blocks[index]);
                        let order = &mut direct_order[state.index()];
                        if *order == u16::MAX {
                            *order = u16::try_from(introductions.len())
                                .expect("state introduction count fits u16");
                            introductions.push(state);
                        }
                        blocks[index] = raw_state_id(state);
                    }
                }
            }
        }
        for &state in &introductions {
            grid.raw_introduction_index.insert(state, ());
        }
        grid.raw_introductions = introductions;
        let cells = (size_x as u64) * (size_y as u64) * (size_z as u64);
        crate::counters::bump_logical_write(
            crate::counters::MemoryBoundary::BlockGrid,
            cells,
            cells * std::mem::size_of::<u16>() as u64,
        );
        grid
    }

    #[inline]
    fn remember_raw_state(&mut self, state: StateId) {
        if !self.raw_introduction_index.contains_key(&state) {
            self.raw_introduction_index.insert(state, ());
            self.raw_introductions.push(state);
        }
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
    #[inline]
    #[must_use]
    pub fn get_id(&self, x: i32, y: i32, z: i32) -> StateId {
        crate::counters::bump_logical_read(crate::counters::MemoryBoundary::BlockGrid, 1, 2);
        self.state_untracked(x, y, z)
    }

    #[inline]
    fn state_untracked(&self, x: i32, y: i32, z: i32) -> StateId {
        match self.index(x, y, z) {
            Some(index) => self.state_at_index(index),
            None => air_state(),
        }
    }

    #[inline]
    fn position_at_index(&self, index: usize) -> (i32, i32, i32) {
        (
            self.min_x + (index % self.size_x as usize) as i32,
            self.min_y + (index / (self.size_x as usize * self.size_z as usize)) as i32,
            self.min_z + (index / self.size_x as usize % self.size_z as usize) as i32,
        )
    }

    #[inline]
    fn borrowed_base_at_index(&self, region: &BorrowedRegion, index: usize) -> StateId {
        let (x, y, z) = self.position_at_index(index);
        let slot = ((x - self.min_x) / 16 * 3 + (z - self.min_z) / 16) as usize;
        crate::counters::bump_end_region_base_read();
        region.bases[slot].state_untracked(x, y, z)
    }

    #[inline]
    fn state_at_index(&self, index: usize) -> StateId {
        match &self.storage {
            GridStorage::Indexed(cells) => self.palette[cells[index] as usize],
            GridStorage::Raw(cells) => StateId::from_raw(cells[index]),
            GridStorage::BorrowedRegion(region) => self.borrowed_state_at_index(region, index),
        }
    }

    #[inline(never)]
    fn borrowed_state_at_index(&self, region: &BorrowedRegion, index: usize) -> StateId {
        region.writes.get(&index).copied()
            .unwrap_or_else(|| self.borrowed_base_at_index(region, index))
    }

    #[inline]
    pub(crate) fn get_id_and_facts(&self, x: i32, y: i32, z: i32) -> (StateId, BaseStateFacts) {
        crate::counters::bump_logical_read(crate::counters::MemoryBoundary::BlockGrid, 1, 2);
        match (&self.storage, self.index(x, y, z)) {
            (GridStorage::Indexed(cells), Some(i)) => {
                let entry = cells[i] as usize;
                (self.palette[entry], self.palette_base_facts[entry])
            }
            (_, Some(i)) => {
                let state = self.state_at_index(i);
                (state, base_facts(state))
            }
            (_, None) => (air_state(), BaseStateFacts::air()),
        }
    }

    /// Base-state id at `(x, y, z)`, with air outside the box.
    /// This is the numeric counterpart of stripping a state string at `'['`.
    #[inline]
    #[must_use]
    pub fn get_base_id(&self, x: i32, y: i32, z: i32) -> StateId {
        crate::counters::bump_logical_read(crate::counters::MemoryBoundary::BlockGrid, 1, 2);
        match (&self.storage, self.index(x, y, z)) {
            (GridStorage::Indexed(cells), Some(i)) => self.palette_bases[cells[i] as usize],
            (_, Some(i)) => base_state(self.state_at_index(i)),
            (_, None) => air_state(),
        }
    }

    /// Typed physical facts for the canonical state at `(x, y, z)`.
    #[inline]
    #[must_use]
    pub fn get_base_facts(&self, x: i32, y: i32, z: i32) -> BaseStateFacts {
        crate::counters::bump_logical_read(crate::counters::MemoryBoundary::BlockGrid, 1, 2);
        self.base_facts_untracked(x, y, z)
    }

    #[inline]
    pub(crate) fn base_facts_untracked(&self, x: i32, y: i32, z: i32) -> BaseStateFacts {
        match (&self.storage, self.index(x, y, z)) {
            (GridStorage::Indexed(cells), Some(i)) => self.palette_base_facts[cells[i] as usize],
            (_, Some(i)) => base_facts(self.state_at_index(i)),
            (_, None) => BaseStateFacts::air(),
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

    /// Writes the canonical `state` at `(x, y, z)`. A no-op outside the box.
    #[inline]
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

    #[inline]
    fn set_id_at_index(&mut self, i: usize, state: StateId) {
        let previous = self.change_capture.as_ref().map(|_| self.state_at_index(i));
        if let Some(changes) = &mut self.change_capture {
            let previous = previous.expect("active capture has a previous state");
            if previous != state {
                changes.entry(i).or_insert(previous);
            }
        }
        if self.storage.is_borrowed() {
            self.set_borrowed_id_at_index(i, state);
        } else if self.storage.is_raw() {
            self.remember_raw_state(state);
            Arc::make_mut(self.storage.dense_cells_mut())[i] =
                raw_state_id(state);
        } else {
            let id = self.palette_index(state);
            Arc::make_mut(self.storage.dense_cells_mut())[i] = id;
        }
        crate::counters::bump_logical_write(crate::counters::MemoryBoundary::BlockGrid, 1, 2);
    }

    #[inline(never)]
    fn set_borrowed_id_at_index(&mut self, index: usize, state: StateId) {
        let GridStorage::BorrowedRegion(region) = &self.storage else { unreachable!() };
        let original = self.borrowed_base_at_index(region, index);
        self.palette_index(state);
        let GridStorage::BorrowedRegion(region) = &mut self.storage else { unreachable!() };
        if state == original {
            region.writes.remove(&index);
        } else if region.writes.insert(index, state).is_none() {
            crate::counters::bump_end_region_overlay_entry();
        }
    }

    pub(crate) fn begin_change_capture(&mut self) {
        assert!(self.change_capture.is_none(), "dense grid change capture is already active");
        self.change_capture = Some(FastMap::default());
    }

    pub(crate) fn finish_change_capture(&mut self) -> Vec<DenseBlockChange> {
        let mut originals = self.change_capture
            .take()
            .expect("dense grid change capture is not active")
            .into_iter()
            .collect::<Vec<_>>();
        originals.sort_unstable_by_key(|&(index, _)| index);
        originals.into_iter()
            .filter_map(|(index, original)| {
                let (x, y, z) = self.position_at_index(index);
                let state = self.get_id(x, y, z);
                (state != original).then_some(DenseBlockChange { position: (x, y, z), state })
            })
            .collect()
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
    /// in first-write order. Outside change capture, each x row uses direct slice indexing instead of
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
        if self.change_capture.is_some()
            || self.storage.is_borrowed()
            || source.storage.is_borrowed()
            || self.storage.is_raw() != source.storage.is_raw()
        {
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
            for y in 0..size_y {
                for z in 0..size_z {
                    for x in 0..size_x {
                        let state = source.get_id(source_x + x, source_y + y, source_z + z);
                        self.set_id(destination_x + x, destination_y + y, destination_z + z, state);
                    }
                }
            }
            return;
        }
        if self.storage.is_raw() && source.storage.is_raw() {
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
            let mut seen = vec![false; block_states::STATE_COUNT as usize];
            let mut introductions = Vec::new();
            let destination_offset_x = destination_x - self.min_x;
            let destination_offset_y = destination_y - self.min_y;
            let destination_offset_z = destination_z - self.min_z;
            let destination_size_x = self.size_x;
            let destination_size_z = self.size_z;
            let destination_blocks = Arc::make_mut(self.storage.dense_cells_mut());
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
                    let source_row = &source.storage.dense_cells()[source_start..source_start + width];
                    for &raw in source_row {
                        let state = StateId::from_raw(raw);
                        let mark = &mut seen[state.index()];
                        if !*mark {
                            *mark = true;
                            introductions.push(state);
                        }
                    }
                    destination_blocks[destination_start..destination_start + width]
                        .copy_from_slice(source_row);
                }
            }
            for state in introductions {
                self.remember_raw_state(state);
            }
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
        let destination_blocks = Arc::make_mut(self.storage.dense_cells_mut());
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
                let source_row = &source.storage.dense_cells()[source_start..source_start + width];
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

    /// Imports borrowed column-palette indices into a 16-wide dense field.
    /// Cells omitted by `visit` remain air. Visit cells in first-encounter order
    /// to preserve the palette order; each source palette entry is resolved once.
    #[must_use]
    pub fn from_column_palette_indices(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        height: i32,
        source_palette: &[StateId],
        visit: impl FnOnce(&mut dyn FnMut(usize, u16)),
    ) -> Self {
        assert!(height >= 0, "grid height is negative");
        let mut grid = Self::with_default(min_x, min_y, min_z, 16, height, 16, air_state());
        let mut remap = vec![u16::MAX; source_palette.len()];
        for (id, &state) in source_palette.iter().enumerate() {
            if state == air_state() {
                remap[id] = 0;
            }
        }
        let blocks = Arc::get_mut(grid.storage.dense_cells_mut()).expect("new grid carrier must be uniquely owned");
        let mut copied = 0u64;
        visit(&mut |cell, source_id| {
            let mapped = &mut remap[source_id as usize];
            if *mapped == u16::MAX {
                *mapped = palette_index(
                    &mut grid.palette,
                    &mut grid.palette_bases,
                    &mut grid.palette_base_facts,
                    &mut grid.index_of,
                    source_palette[source_id as usize],
                );
            }
            blocks[cell] = *mapped;
            copied += 1;
        });
        crate::counters::bump_logical_write(
            crate::counters::MemoryBoundary::BlockGrid,
            copied,
            copied * std::mem::size_of::<u16>() as u64,
        );
        grid
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
        let mut out = HashMap::with_capacity(self.cell_count());
        for ly in 0..self.size_y {
            for lz in 0..self.size_z {
                for lx in 0..self.size_x {
                    let state = self.get_id(self.min_x + lx, self.min_y + ly, self.min_z + lz).canonical_state();
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
        let (palette, blocks) = self.into_id_palette_and_blocks();
        (
            palette.into_iter().map(|state| state.canonical_state()).collect(),
            blocks,
        )
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
        assert!(
            self.index(min_x, min_y, min_z).is_some()
                && self
                    .index(min_x + size_x - 1, min_y + size_y - 1, min_z + size_z - 1)
                    .is_some(),
            "output box is outside the grid",
        );

        let cell_count = (size_x as usize) * (size_y as usize) * (size_z as usize);
        let state_count = if self.storage.is_raw() { self.raw_introductions.len() } else { self.palette.len() };
        let capacity = state_count.min(cell_count + 1).max(1);
        let mut palette = Vec::with_capacity(capacity);
        let mut index_of = FastMap::with_capacity_and_hasher(capacity.max(1), Default::default());
        palette.push(default);
        index_of.insert(default, 0);

        let mut blocks = Vec::with_capacity(cell_count);
        for y in min_y..min_y + size_y {
            for z in min_z..min_z + size_z {
                for x in min_x..min_x + size_x {
                    let source_state = self.get_id(x, y, z);
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
        match self.into_state_lane_parts() {
            DenseBlockGridParts::Indexed { palette, blocks } => (palette, blocks),
            DenseBlockGridParts::Raw { introductions, states } => {
                let (palette, blocks) = remap_raw_state_lane(introductions, Arc::unwrap_or_clone(states));
                (palette, Arc::new(blocks))
            }
        }
    }

    pub(crate) fn into_state_lane_parts(self) -> DenseBlockGridParts {
        if self.storage.is_borrowed() {
            return self.materialize_region().into_state_lane_parts();
        }
        let cells = self.cell_count();
        crate::counters::bump_logical_read(
            crate::counters::MemoryBoundary::BlockGrid,
            cells as u64,
            (cells * std::mem::size_of::<u16>()) as u64,
        );
        match self.storage {
            GridStorage::Raw(states) => DenseBlockGridParts::Raw {
                introductions: self.raw_introductions,
                states,
            },
            GridStorage::Indexed(blocks) => DenseBlockGridParts::Indexed {
                palette: self.palette,
                blocks,
            },
            GridStorage::BorrowedRegion(_) => unreachable!(),
        }
    }

    fn cell_count(&self) -> usize {
        self.size_x.max(0) as usize * self.size_y.max(0) as usize * self.size_z.max(0) as usize
    }

    /// Contiguous consumers explicitly pay the source copy once. Replay
    /// consumers use point reads and never cross this boundary.
    fn materialize_region(self) -> Self {
        let mut dense = Self::with_default(
            self.min_x, self.min_y, self.min_z, self.size_x, self.size_y, self.size_z, air_state(),
        );
        let GridStorage::BorrowedRegion(region) = self.storage else { unreachable!() };
        for base in &region.bases {
            let min_y = self.min_y.max(base.min_y);
            let max_y = (self.min_y + self.size_y).min(base.min_y + base.size_y);
            let height = (max_y - min_y).max(0);
            dense.copy_box_from(
                base, base.min_x, min_y, base.min_z, base.min_x, min_y, base.min_z, 16, height, 16,
            );
            crate::counters::bump_end_region_base_copy(16 * height as u64 * 16);
        }
        for state in self.palette {
            dense.palette_index(state);
        }
        for (index, state) in region.writes {
            let x = self.min_x + (index % self.size_x as usize) as i32;
            let z = self.min_z + (index / self.size_x as usize % self.size_z as usize) as i32;
            let y = self.min_y + (index / (self.size_x as usize * self.size_z as usize)) as i32;
            dense.set_id(x, y, z, state);
        }
        dense.change_capture = self.change_capture;
        dense
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
        assert!(
            self.index(min_x, min_y, min_z).is_some()
                && self.index(min_x + size_x - 1, min_y + size_y - 1, min_z + size_z - 1).is_some(),
            "output box is outside the grid",
        );
        let cell_count = (size_x as usize) * (size_y as usize) * (size_z as usize);
        let state_count = if self.storage.is_raw() { self.raw_introductions.len() } else { self.palette.len() };
        let capacity = state_count.min(cell_count + 1).max(1);
        let mut palette = Vec::with_capacity(capacity);
        let mut index_of = vec![u16::MAX; block_states::STATE_COUNT as usize];
        palette.push(default);
        index_of[default.index()] = 0;
        let mut blocks = Vec::with_capacity(cell_count);
        for y in min_y..min_y + size_y {
            for z in min_z..min_z + size_z {
                for x in min_x..min_x + size_x {
                    let state = self.get_id(x, y, z);
                    let local = if index_of[state.index()] != u16::MAX {
                        index_of[state.index()]
                    } else {
                        let local = u16::try_from(palette.len())
                            .expect("more than 65,536 palette entries in one output box");
                        palette.push(state);
                        index_of[state.index()] = local;
                        local
                    };
                    blocks.push(local);
                }
            }
        }
        (palette, blocks)
    }

    /// Folds a 16-by-16 crop directly into compact sections in final-cell
    /// Y/Z/X encounter order. Rows above `source_height` are output padding
    /// filled with `default`, not reads of the source grid.
    #[must_use]
    pub fn into_compact_column_box(
        self,
        min_x: i32,
        min_y: i32,
        min_z: i32,
        source_height: i32,
        output_height: i32,
        default: StateId,
    ) -> (Vec<StateId>, CompactBlockStorage) {
        assert!(source_height >= 0 && output_height >= source_height, "invalid compact crop height");
        if source_height > 0 {
            assert!(
                self.index(min_x, min_y, min_z).is_some()
                    && self.index(min_x + 15, min_y + source_height - 1, min_z + 15).is_some(),
                "output box is outside the grid",
            );
        }
        let source_cells = source_height as usize * 256;
        crate::counters::bump_logical_read(
            crate::counters::MemoryBoundary::BlockGrid,
            source_cells as u64,
            source_cells as u64 * std::mem::size_of::<u16>() as u64,
        );
        let state_count = if self.storage.is_raw() { self.raw_introductions.len() } else { self.palette.len() };
        let mut palette = Vec::with_capacity(state_count.min(source_cells + 1).max(1));
        let mut index_of = vec![u16::MAX; block_states::STATE_COUNT as usize];
        palette.push(default);
        index_of[default.index()] = 0;
        let (blocks, _) = CompactBlockStorage::from_section_fn_with_predicates(
            min_y, output_height, 1, false, None, None, [u16::MAX; 2],
            |section, cells| {
                let start = section * 16 * 256;
                if start >= source_cells {
                    return Some(0);
                }
                let copied = cells.len().min(source_cells - start);
                cells[copied..].fill(0);
                let mut first = 0;
                let mut uniform = true;
                for (offset, cell) in cells.iter_mut().take(copied).enumerate() {
                    let index = start + offset;
                    let state = self.get_id(
                        min_x + (index % 16) as i32,
                        min_y + (index / 256) as i32,
                        min_z + (index / 16 % 16) as i32,
                    );
                    let id = if index_of[state.index()] != u16::MAX {
                        index_of[state.index()]
                    } else {
                        let id = u16::try_from(palette.len())
                            .expect("more than 65,536 palette entries in one output box");
                        palette.push(state);
                        index_of[state.index()] = id;
                        id
                    };
                    *cell = id;
                    if offset == 0 {
                        first = id;
                    } else {
                        uniform &= id == first;
                    }
                }
                uniform &= copied == cells.len() || first == 0;
                uniform.then_some(first)
            },
        );
        (palette, blocks)
    }
}

fn remap_raw_state_lane(introductions: Vec<StateId>, mut states: Vec<u16>) -> (Vec<StateId>, Vec<u16>) {
    let mut state_to_palette = vec![u16::MAX; block_states::STATE_COUNT as usize];
    for (palette_index, state) in introductions.iter().copied().enumerate() {
        state_to_palette[state.index()] =
            u16::try_from(palette_index).expect("state palette index fits u16");
    }
    for raw in &mut states {
        let state = StateId::from_raw(*raw);
        let index = state_to_palette[state.index()];
        assert_ne!(index, u16::MAX, "raw cell state is missing its palette introduction");
        *raw = index;
    }
    (introductions, states)
}

impl crate::structure::StructureWorld for DenseBlockGrid {
    fn bounds(&self) -> (i32, i32, i32, i32, i32, i32) {
        DenseBlockGrid::bounds(self)
    }

    fn get_id(&self, x: i32, y: i32, z: i32) -> StateId {
        DenseBlockGrid::get_id(self, x, y, z)
    }

    fn set_id(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        DenseBlockGrid::set_id(self, x, y, z, state);
    }

    fn base_facts(&self, x: i32, y: i32, z: i32) -> BaseStateFacts {
        self.base_facts_untracked(x, y, z)
    }

    fn set_id_observed(
        &mut self, x: i32, y: i32, z: i32, state: StateId,
        source: (i32, i32), step: i32, sink: &mut dyn StructureMutationSink,
    ) {
        DenseBlockGrid::set_id_observed(self, x, y, z, state, source, step, sink);
    }
}

#[inline]
fn raw_state_id(state: StateId) -> u16 {
    debug_assert!(state.raw() <= u16::MAX as u32);
    state.raw() as u16
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

    #[test]
    fn compact_crop_preserves_final_encounter_order_and_partial_padding() {
        let air = StateId::AIR;
        let stone = state("minecraft:stone");
        let sand = state("minecraft:sand");
        let water = state("minecraft:water");
        let copper = state("minecraft:copper_ore");
        let gold = state("minecraft:gold_block");
        for raw in [false, true] {
            let mut grid = if raw {
                DenseBlockGrid::with_default_raw(-20, -5, 7, 20, 21, 19, air)
            } else {
                DenseBlockGrid::with_default(-20, -5, 7, 20, 21, 19, air)
            };
            grid.set_id(-20, -5, 7, gold);
            grid.set_id(-18, -4, 9, water);
            grid.set_id(-17, -4, 8, sand);
            grid.set_id(-18, -4, 8, copper);
            grid.set_id(-18, -4, 8, stone);
            grid.set_id(-3, 12, 23, stone);
            let flat = grid.clone().into_id_palette_and_blocks_box(-18, -4, 8, 16, 17, 16, air);
            let (palette, blocks) = grid.into_compact_column_box(-18, -4, 8, 17, 35, air);
            assert_eq!(palette, [air, stone, sand, water]);
            assert_ne!(palette, [air, water, sand, copper, stone]);
            assert!(!palette.contains(&copper) && !palette.contains(&gold));
            assert_eq!(blocks.min_y(), -4);
            assert_eq!(blocks.height(), 35);
            assert_eq!(blocks.section_rows(2), 3);
            assert_eq!(blocks.section(2).unwrap().uniform_id(), Some(0));
            let mut expected = vec![0u16; 35 * 256];
            expected[0] = 1;
            expected[1] = 2;
            expected[16] = 3;
            expected[16 * 256 + 255] = 1;
            assert_eq!(flat.0, palette);
            assert_eq!(flat.1, expected[..17 * 256]);
            assert_eq!(blocks.into_flat(), expected);
        }
    }

    #[test]
    fn compact_crop_uniform_and_empty_rows_have_exact_extent() {
        let stone = state("minecraft:stone");
        let air = StateId::AIR;
        let grid = DenseBlockGrid::with_default(-16, -7, 32, 16, 17, 16, stone);
        let (palette, blocks) = grid.clone().into_compact_column_box(-16, -7, 32, 17, 17, air);
        assert_eq!(palette, [air, stone]);
        assert_eq!(blocks.section(0).unwrap().uniform_id(), Some(1));
        assert_eq!(blocks.section(1).unwrap().uniform_id(), Some(1));
        assert_eq!(blocks.into_flat(), vec![1; 17 * 256]);
        let (palette, mut padded) = grid.clone().into_compact_column_box(-16, -7, 32, 17, 35, air);
        assert_eq!(palette, [air, stone]);
        assert_eq!(padded.get(15, 9, 15), 1);
        assert_eq!(padded.get(15, 10, 15), 0);
        assert_eq!(padded.section(1).unwrap().uniform_id(), None);
        padded.set(15, 27, 15, 1);
        assert_eq!(padded.get(15, 27, 15), 1);
        padded.set(15, 27, 15, 0);
        assert_eq!(padded.get(15, 27, 15), 0);
        let (palette, empty) = grid.into_compact_column_box(-16, -7, 32, 0, 19, air);
        assert_eq!(palette, [air]);
        assert_eq!(empty.into_flat(), vec![0; 19 * 256]);
    }

    #[test]
    fn borrowed_region_matches_dense_bounds_palette_and_ordered_writes() {
        let stone = state("minecraft:stone");
        let dirt = state("minecraft:dirt");
        let water = state("minecraft:water[level=3]");
        let gold = state("minecraft:gold_block");
        let (min_x, min_y, min_z, height) = (-32, -3, 16, 7);
        let bases = std::array::from_fn(|slot| {
            let x = min_x + (slot / 3) as i32 * 16;
            let z = min_z + (slot % 3) as i32 * 16;
            let default = if slot % 2 == 0 { stone } else { dirt };
            let mut base = if slot % 2 == 0 {
                DenseBlockGrid::with_default(x, -2, z, 16, (slot % 3) as i32 + 2, 16, default)
            } else {
                DenseBlockGrid::with_default_raw(x, -2, z, 16, (slot % 3) as i32 + 2, 16, default)
            };
            base.set_id(x + 4, -2, z + 4, water);
            Arc::new(base)
        });
        let mut borrowed = DenseBlockGrid::borrowed_region(min_x, min_y, min_z, height, bases.clone());
        let mut dense = DenseBlockGrid::with_default(min_x, min_y, min_z, 48, height, 48, StateId::AIR);
        for base in &bases {
            dense.copy_box_from(
                base, base.min_x, base.min_y, base.min_z, base.min_x, base.min_y, base.min_z,
                16, base.size_y, 16,
            );
        }
        for y in min_y..min_y + height {
            for z in min_z..min_z + 48 {
                for x in min_x..min_x + 48 {
                    let slot = ((x - min_x) / 16 * 3 + (z - min_z) / 16) as usize;
                    let expected = if y < -2 || y >= (slot % 3) as i32 {
                        StateId::AIR
                    } else if y == -2 && (x - min_x) % 16 == 4 && (z - min_z) % 16 == 4 {
                        water
                    } else if slot % 2 == 0 { stone } else { dirt };
                    assert_eq!(borrowed.get_id(x, y, z), expected, "cell ({x},{y},{z})");
                    assert_eq!(borrowed.get_id_and_facts(x, y, z), dense.get_id_and_facts(x, y, z));
                    assert_eq!(borrowed.get_base_id(x, y, z), dense.get_base_id(x, y, z));
                    assert_eq!(borrowed.get_base_facts(x, y, z), dense.get_base_facts(x, y, z));
                }
            }
        }
        let seeded = (min_x + 1, -3, min_z + 1);
        let restored = (min_x + 2, -1, min_z + 2);
        let changed = (min_x + 17, -1, min_z + 18);
        let high = (min_x + 33, 3, min_z + 34);
        let mut borrowed_recorder = crate::structure::StructureMutationRecorder::default();
        let mut dense_recorder = crate::structure::StructureMutationRecorder::default();
        for grid in [&mut borrowed, &mut dense] {
            grid.set_id(seeded.0, seeded.1, seeded.2, gold);
            grid.set_id(min_x - 1, -3, min_z, gold);
            grid.begin_change_capture();
        }
        for (position, value) in [
            (high, dirt), (changed, water), (seeded, dirt), (seeded, gold),
            (restored, dirt), (restored, stone), (restored, stone),
        ] {
            borrowed.set_id_observed(position.0, position.1, position.2, value, (8, -5), 3, &mut borrowed_recorder);
            dense.set_id_observed(position.0, position.1, position.2, value, (8, -5), 3, &mut dense_recorder);
        }
        let expected_changes = [
            DenseBlockChange { position: changed, state: water },
            DenseBlockChange { position: high, state: dirt },
        ];
        assert_eq!(borrowed.finish_change_capture(), expected_changes);
        assert_eq!(dense.finish_change_capture(), expected_changes);
        let provenance = borrowed_recorder.finish();
        assert_eq!(provenance, dense_recorder.finish());
        assert_eq!(provenance.mutations().len(), 7);
        assert_eq!(provenance.mutations().iter().map(|write| write.ordinal).collect::<Vec<_>>(), (0..7).collect::<Vec<_>>());
        assert_eq!(borrowed.get_id(min_x - 1, -3, min_z), StateId::AIR);
        let GridStorage::BorrowedRegion(region) = &borrowed.storage else { panic!("region materialized during replay") };
        assert_eq!(region.writes.len(), 3);
        for (actual, original) in region.bases.iter().zip(&bases) {
            assert!(Arc::ptr_eq(actual, original));
        }
        let snapshot = borrowed.clone();
        borrowed.set_id(seeded.0, seeded.1, seeded.2, dirt);
        assert_eq!(snapshot.get_id(seeded.0, seeded.1, seeded.2), gold);
        assert_eq!(bases[0].get_id(seeded.0, seeded.1, seeded.2), StateId::AIR);
        assert_eq!(snapshot.into_id_palette_and_blocks(), dense.into_id_palette_and_blocks());
        let mut copied = DenseBlockGrid::with_default(min_x, min_y, min_z, 2, 2, 2, StateId::AIR);
        copied.copy_box_from(&borrowed, min_x, -3, min_z, min_x, -3, min_z, 2, 2, 2);
        assert_eq!(copied.get_id(seeded.0, seeded.1, seeded.2), dirt);
        copied.set_id(seeded.0, seeded.1, seeded.2, water);
        borrowed.copy_box_from(&copied, min_x, -3, min_z, min_x, -3, min_z, 2, 2, 2);
        assert_eq!(borrowed.get_id(seeded.0, seeded.1, seeded.2), water);
        assert!(borrowed.storage.is_borrowed());
    }

    #[test]
    fn change_capture_orders_net_changes_without_cloning_the_carrier() {
        let stone = state("minecraft:stone");
        let dirt = state("minecraft:dirt");
        let gold = state("minecraft:gold_block");
        let water = state("minecraft:water");
        for mut grid in [
            DenseBlockGrid::with_default(-7, -5, 11, 3, 4, 2, stone),
            DenseBlockGrid::with_default_raw(-7, -5, 11, 3, 4, 2, stone),
        ] {
            grid.set_id(-6, -4, 12, dirt);
            let carrier = Arc::as_ptr(grid.storage.dense_cells());
            grid.begin_change_capture();
            assert_eq!(Arc::strong_count(grid.storage.dense_cells()), 1);
            for (x, y, z, value) in [
                (-5, -2, 12, gold), (-6, -3, 11, dirt), (-7, -5, 12, water),
                (-6, -4, 12, stone), (-5, -5, 11, gold), (-6, -5, 11, dirt),
                (-6, -5, 11, stone), (-7, -5, 11, stone),
                (-8, -5, 11, gold), (-4, -5, 11, gold), (-7, -6, 11, gold),
                (-7, -1, 11, gold), (-7, -5, 10, gold), (-7, -5, 13, gold),
            ] {
                grid.set_id(x, y, z, value);
            }
            assert_eq!(grid.finish_change_capture(), [
                DenseBlockChange { position: (-5, -5, 11), state: gold },
                DenseBlockChange { position: (-7, -5, 12), state: water },
                DenseBlockChange { position: (-6, -4, 12), state: stone },
                DenseBlockChange { position: (-6, -3, 11), state: dirt },
                DenseBlockChange { position: (-5, -2, 12), state: gold },
            ]);
            assert_eq!(Arc::as_ptr(grid.storage.dense_cells()), carrier);
            grid.begin_change_capture();
            assert!(grid.finish_change_capture().is_empty());
        }
    }

    #[test]
    fn change_capture_clones_continue_independently() {
        let stone = state("minecraft:stone");
        let dirt = state("minecraft:dirt");
        let water = state("minecraft:water");
        for mut grid in [
            DenseBlockGrid::with_default(-7, -5, 11, 3, 4, 2, stone),
            DenseBlockGrid::with_default_raw(-7, -5, 11, 3, 4, 2, stone),
        ] {
            grid.begin_change_capture();
            grid.set_id(-5, -2, 12, dirt);
            let mut clone = grid.clone();
            grid.set_id(-7, -5, 11, water);
            clone.set_id(-5, -2, 12, stone);
            clone.set_id(-6, -4, 12, water);
            assert_eq!(grid.finish_change_capture(), [
                DenseBlockChange { position: (-7, -5, 11), state: water },
                DenseBlockChange { position: (-5, -2, 12), state: dirt },
            ]);
            assert_eq!(clone.finish_change_capture(), [
                DenseBlockChange { position: (-6, -4, 12), state: water },
            ]);
            assert_eq!(grid.get_id(-6, -4, 12), stone);
            assert_eq!(clone.get_id(-7, -5, 11), stone);
        }
    }

    #[test]
    fn change_capture_includes_every_bulk_copy_lane_combination() {
        let stone = state("minecraft:stone");
        let dirt = state("minecraft:dirt");
        let water = state("minecraft:water");
        let gold = state("minecraft:gold_block");
        for source_raw in [false, true] {
            for destination_raw in [false, true] {
                let mut source = if source_raw {
                    DenseBlockGrid::with_default_raw(20, 40, -9, 2, 2, 2, stone)
                } else {
                    DenseBlockGrid::with_default(20, 40, -9, 2, 2, 2, stone)
                };
                source.set_id(21, 40, -9, dirt);
                source.set_id(20, 41, -8, water);
                let mut grid = if destination_raw {
                    DenseBlockGrid::with_default_raw(-7, -5, 11, 3, 4, 2, stone)
                } else {
                    DenseBlockGrid::with_default(-7, -5, 11, 3, 4, 2, stone)
                };
                grid.set_id(-7, -4, 11, gold);
                grid.set_id(-6, -4, 11, dirt);
                let mut uncaptured = grid.clone();
                grid.begin_change_capture();
                grid.copy_box_from(&source, 20, 40, -9, -7, -4, 11, 2, 2, 2);
                assert_eq!(grid.finish_change_capture(), [
                    DenseBlockChange { position: (-7, -4, 11), state: stone },
                    DenseBlockChange { position: (-7, -3, 12), state: water },
                ], "source raw={source_raw}, destination raw={destination_raw}");
                uncaptured.copy_box_from(&source, 20, 40, -9, -7, -4, 11, 2, 2, 2);
                assert_eq!(grid.into_id_palette_and_blocks(), uncaptured.into_id_palette_and_blocks());
            }
        }
    }

    #[test]
    fn change_capture_cancellation_retains_structure_provenance() {
        let stone = state("minecraft:stone");
        let dirt = state("minecraft:dirt");
        for mut grid in [
            DenseBlockGrid::with_default(-7, -5, 11, 3, 4, 2, stone),
            DenseBlockGrid::with_default_raw(-7, -5, 11, 3, 4, 2, stone),
        ] {
            let mut recorder = crate::structure::StructureMutationRecorder::default();
            grid.begin_change_capture();
            for value in [dirt, stone, stone] {
                grid.set_id_observed(-6, -4, 12, value, (4, -3), 7, &mut recorder);
            }
            assert!(grid.finish_change_capture().is_empty());
            let blocks = recorder.finish();
            assert_eq!(blocks.mutations().iter().map(|write| {
                (write.source, write.step, write.ordinal, write.position, write.state)
            }).collect::<Vec<_>>(), [
                ((4, -3), 7, 0, [-6, -4, 12], dirt),
                ((4, -3), 7, 1, [-6, -4, 12], stone),
                ((4, -3), 7, 2, [-6, -4, 12], stone),
            ]);
        }
    }

    #[test]
    #[should_panic(expected = "dense grid change capture is already active")]
    fn change_capture_rejects_nested_capture() {
        let mut grid = DenseBlockGrid::with_default(0, 0, 0, 1, 1, 1, StateId::AIR);
        grid.begin_change_capture();
        grid.begin_change_capture();
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn change_capture_detector_rejects_a_bypassed_writer() {
        let dirt = state("minecraft:dirt");
        let mut grid = DenseBlockGrid::with_default(-7, -5, 11, 1, 1, 1, StateId::AIR);
        grid.begin_change_capture();
        let dirt_index = grid.palette_index(dirt);
        Arc::make_mut(grid.storage.dense_cells_mut())[0] = dirt_index;
        assert_eq!(grid.finish_change_capture(), [
            DenseBlockChange { position: (-7, -5, 11), state: dirt },
        ]);
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn change_capture_carrier_detector_rejects_a_baseline_clone() {
        let mut grid = DenseBlockGrid::with_default(0, 0, 0, 1, 1, 1, StateId::AIR);
        let carrier = Arc::as_ptr(grid.storage.dense_cells());
        let baseline = grid.clone();
        grid.begin_change_capture();
        grid.set_id(0, 0, 0, state("minecraft:dirt"));
        assert_eq!(Arc::as_ptr(grid.storage.dense_cells()), carrier);
        assert_eq!(baseline.get_id(0, 0, 0), StateId::AIR);
    }

    #[test]
    fn column_palette_import_remaps_repeated_states_in_encounter_order() {
        let air = StateId::AIR;
        let stone = state("minecraft:stone");
        let dirt = state("minecraft:dirt");
        let water = state("minecraft:water");
        let source_palette = [air, dirt, state("minecraft:lava"), stone, stone, water];
        let grid = DenseBlockGrid::from_column_palette_indices(
            -32, -7, -48, 3, &source_palette,
            |write| {
                for (cell, id) in [(1, 3), (17, 1), (255, 3), (258, 4), (527, 5)] {
                    write(cell, id);
                }
            },
        );
        for (x, y, z, expected) in [
            (-31, -7, -48, stone),
            (-31, -7, -47, dirt),
            (-17, -7, -33, stone),
            (-30, -6, -48, stone),
            (-17, -5, -48, water),
            (-32, -7, -48, air),
            (-31, -8, -48, air),
        ] {
            assert_eq!(grid.get_id(x, y, z), expected, "cell ({x}, {y}, {z})");
        }
        let mut expected_blocks = vec![0; 768];
        expected_blocks[1] = 1;
        expected_blocks[17] = 2;
        expected_blocks[255] = 1;
        expected_blocks[258] = 1;
        expected_blocks[527] = 3;
        let expected = (vec![air, stone, dirt, water], expected_blocks);
        let actual = grid.into_id_palette_and_blocks();
        assert_eq!(actual, expected);

        let mut wrong_order = expected.clone();
        wrong_order.0.swap(1, 2);
        assert_ne!(actual.0, wrong_order.0);
        let mut wrong_stride = expected;
        wrong_stride.1.swap(258, 273);
        assert_ne!(actual.1[258], wrong_stride.1[258]);
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn column_palette_import_order_detector_rejects_changed_expected_order() {
        let stone = state("minecraft:stone");
        let dirt = state("minecraft:dirt");
        let grid = DenseBlockGrid::from_column_palette_indices(
            -32, -7, -48, 1, &[StateId::AIR, dirt, stone],
            |write| {
                write(1, 2);
                write(17, 1);
            },
        );
        let (palette, _) = grid.into_id_palette_and_blocks();
        assert_eq!(palette, [StateId::AIR, dirt, stone]);
    }

    #[test]
    fn column_palette_import_omitted_cells_and_unused_palette_remain_air() {
        let grid = DenseBlockGrid::from_column_palette_indices(
            -16, -19, 32, 17,
            &[state("minecraft:stone"), StateId::AIR],
            |_| {},
        );
        assert_eq!(grid.into_id_palette_and_blocks(), (vec![StateId::AIR], vec![0; 4352]));
    }

    #[test]
    fn combined_id_and_facts_preserve_raw_and_indexed_states_and_bounds() {
        let coral = state("minecraft:brain_coral_fan[waterlogged=true]");
        let stone = state("minecraft:stone");
        for mut grid in [
            DenseBlockGrid::with_default(-16, -3, -32, 2, 4, 2, stone),
            DenseBlockGrid::with_default_raw(-16, -3, -32, 2, 4, 2, stone),
        ] {
            grid.set_id(-15, -1, -31, coral);
            assert_eq!(
                grid.get_id_and_facts(-15, -1, -31),
                (coral, BaseStateFacts::Builtin {
                    is_air: false, is_fluid: true, blocks_motion: false,
                }),
            );
            assert_eq!(
                grid.get_id_and_facts(-16, -3, -32),
                (stone, BaseStateFacts::Builtin {
                    is_air: false, is_fluid: false, blocks_motion: true,
                }),
            );
            for (x, y, z) in [
                (-17, -1, -31), (-14, -1, -31), (-15, -4, -31),
                (-15, 1, -31), (-15, -1, -33), (-15, -1, -30),
            ] {
                assert_eq!(grid.get_id_and_facts(x, y, z), (StateId::AIR, BaseStateFacts::air()));
            }
        }
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
            "minecraft:water[level=0]".to_owned(),
            "minecraft:lava[level=0]".to_owned(),
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
        assert_eq!(digest, 0x5917_d9f3_67b8_9eae, "packed output digest changed");

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
    fn raw_lane_preserves_palette_history_and_box_fold_contracts() {
        let air = state("minecraft:air");
        let stone = state("minecraft:stone");
        let water = state("minecraft:water");
        let sand = state("minecraft:sand");
        let copper_ore = state("minecraft:copper_ore");
        let states = [stone, water, sand, stone, water, sand, air, stone];
        let at = |x: i32, y: i32, z: i32| states[((z * 2 + x) * 2 + y) as usize];
        let mut indexed = DenseBlockGrid::from_ordered_state_fn(0, 0, 0, 2, 2, 2, air, at);
        let mut raw = DenseBlockGrid::from_ordered_state_fn_raw(0, 0, 0, 2, 2, 2, air, at);
        for &(x, y, z, replacement) in &[(0, 0, 0, copper_ore), (0, 0, 0, stone)] {
            indexed.set_id(x, y, z, replacement);
            raw.set_id(x, y, z, replacement);
        }

        assert_eq!(raw.get_id(0, 1, 0), indexed.get_id(0, 1, 0));
        assert_eq!(raw.get_base_id(0, 1, 0), indexed.get_base_id(0, 1, 0));
        assert_eq!(raw.get_base_facts(0, 1, 0), indexed.get_base_facts(0, 1, 0));

        let indexed_box = indexed.clone().into_id_palette_and_blocks_box(0, 0, 0, 2, 2, 2, air);
        let raw_box = raw.clone().into_id_palette_and_blocks_box(0, 0, 0, 2, 2, 2, air);
        assert_eq!(raw_box, indexed_box, "raw box fold changed final-cell palette order");
        assert!(!raw_box.0.contains(&copper_ore), "box fold must omit overwritten transient states");

        let indexed_output = indexed.into_id_palette_and_blocks();
        let raw_output = raw.into_id_palette_and_blocks();
        assert_eq!(raw_output, indexed_output, "raw lane lost overwritten palette introductions");
        assert!(raw_output.0.contains(&copper_ore), "full palette must retain transient state introductions");
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
        let mut g = DenseBlockGrid::new_named(0, 0, 0, 4, 4, 4, "minecraft:air");
        g.set_named(0, 0, 0, "minecraft:granite");
        g.set_named(1, 0, 0, "minecraft:andesite");
        g.set_named(2, 0, 0, "minecraft:granite");
        g.set_named(3, 0, 0, "minecraft:calcite");
        g.set_named(3, 0, 0, "minecraft:stone");
        assert_eq!(g.get_named(3, 0, 0), "minecraft:stone");
        let (palette, _) = g.into_named_palette_and_blocks();
        assert_eq!(
            palette,
            vec![
                "minecraft:air".to_string(),
                "minecraft:granite".to_string(),
                "minecraft:andesite".to_string(),
                "minecraft:calcite".to_string(),
                "minecraft:stone".to_string(),
            ],
        );
    }

    #[test]
    fn an_id_read_from_one_grid_writes_into_another_sharing_canonical_state() {
        let air = state("minecraft:air");
        let mut src = DenseBlockGrid::with_default(0, 0, 0, 2, 2, 2, air);
        let mut dst = DenseBlockGrid::with_default(0, 0, 0, 2, 2, 2, air);
        let deepslate = state("minecraft:deepslate[axis=y]");
        src.set_named(1, 1, 1, "minecraft:deepslate");

        dst.set_id(0, 0, 0, src.get_id(1, 1, 1));

        assert_eq!(src.get_id(1, 1, 1), deepslate);
        assert_eq!(dst.get_id(0, 0, 0), deepslate);
        assert_eq!(dst.get_named(0, 0, 0), deepslate.canonical_state());
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
        let packed = (0..3)
            .flat_map(|y| (0..2).map(move |x| states[(x * 3 + y) as usize].raw() as u16))
            .collect();
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
