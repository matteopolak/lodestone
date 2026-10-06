//! [`VegGrid`]: the read/write surface a structure's feature-pool elements
//! place into, with incremental heightmaps over its own writes.

use std::cell::Cell;

use crate::feature::overlay::{Overlay, WriteLog};
use crate::dense_grid::base_facts;
use crate::structure::StructureWorld;
use lodestone_data::block_states::StateId;

const HEIGHT_CACHE_UNSET: i32 = i32::MIN;

/// The mutable block field vegetal decoration reads and writes.
///
/// Every accessor takes **absolute** world coordinates and translates them
/// through `origin_x`/`origin_z` into a local footprint `[local_lo, local_hi)`
/// on both axes. Writes land in a sparse overlay; a read misses through to the
/// source that owns the column, then to air.
///
/// Production builds one per structure placement pass over the structure's
/// own world ([`Self::with_borrowed_structure_source`]); unit fixtures build a
/// source-less grid and seed cells directly.
#[derive(Debug)]
pub struct VegGrid<'source> {
    /// What decoration wrote, keyed by **local** `(lx, y, lz)`.
    ///
    /// Overlay-first reads are load-bearing: heightmaps update as decoration
    /// places blocks, so a block placed earlier in a pass must be visible to a
    /// later height probe in the same pass.
    blocks: Overlay,
    /// Baseline cells a source-less fixture seeded, so the world-surface WG
    /// lanes see the fixture's terrain rather than an all-air column, and do
    /// not observe a later decoration write.
    #[cfg(test)]
    seeded_baseline: Option<Overlay>,
    /// The terrain a read misses through to. Read-only: writes go to `blocks`
    /// and the caller replays [`Self::dirty_cells`] into the world it owns.
    sources: Option<&'source mut dyn StructureWorld>,
    /// Positions written by `set_id_if_in_bounds`, **local**, in write order.
    /// A `Vec` rather than a map so the replay order is deterministic.
    dirty: WriteLog,
    origin_x: i32,
    origin_z: i32,
    pub(super) min_y: i32,
    pub(super) height: i32,
    local_lo: i32,
    local_hi: i32,
    /// The ids of `minecraft:{air,cave_air,void_air}`, so the world-surface
    /// air test is three integer compares rather than a registry read.
    air_ids: [StateId; 3],
    /// Memoised heightmap results per local `(x, z)` column, five lanes per
    /// cell. A miss walks the column once for the live lanes, or separately for
    /// the immutable WG lanes. Interior-mutable because the height accessors
    /// take `&self`; a write invalidates only the column it touches.
    /// [`HEIGHT_CACHE_UNSET`] is below every build range and marks an
    /// uncomputed lane.
    height_cache: Vec<Cell<[i32; 5]>>,
    local_width: usize,
}

impl<'source> VegGrid<'source> {
    fn empty(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        sources: Option<&'source mut dyn StructureWorld>,
    ) -> Self {
        let air_ids = [
            lodestone_data::block::Block::Air.default_state(),
            lodestone_data::block::Block::CaveAir.default_state(),
            lodestone_data::block::Block::VoidAir.default_state(),
        ];
        let local_width = usize::try_from(local_hi - local_lo)
            .expect("VegGrid footprint must have a non-negative width");
        let column_count = local_width * local_width;
        Self {
            blocks: Overlay::with_bounds(local_lo, local_hi, min_y, height),
            #[cfg(test)]
            seeded_baseline: None,
            sources,
            dirty: WriteLog::default(),
            origin_x,
            origin_z,
            min_y,
            height,
            local_lo,
            local_hi,
            air_ids,
            height_cache: std::iter::repeat_with(|| Cell::new([HEIGHT_CACHE_UNSET; 5]))
                .take(column_count)
                .collect(),
            local_width,
        }
    }

    /// The exclusive source borrow keeps its baseline frozen until this grid
    /// drops; block and height probes only take shared reborrows of it.
    pub(crate) fn with_borrowed_structure_source(source: &'source mut dyn StructureWorld) -> Self {
        let (min_x, min_y, min_z, size_x, size_y, size_z) = source.bounds();
        debug_assert_eq!(size_x, size_z, "structure feature grids are square chunks");
        Self::empty(min_y, size_y, min_x, min_z, 0, size_x, Some(source))
    }
}

#[cfg(test)]
impl VegGrid<'static> {
    /// A source-less fixture over one chunk's footprint (`0..16` local).
    #[must_use]
    pub(crate) fn new(min_y: i32, height: i32, origin_x: i32, origin_z: i32) -> Self {
        Self::with_footprint(min_y, height, origin_x, origin_z, 0, 16)
    }

    /// A source-less fixture with an explicit local footprint `[local_lo, local_hi)`.
    #[must_use]
    pub(crate) fn with_footprint(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
    ) -> Self {
        Self::empty(min_y, height, origin_x, origin_z, local_lo, local_hi, None)
    }
}

#[cfg(test)]
impl VegGrid<'_> {

    /// Seeds a fixture cell: written to the overlay without being recorded as
    /// a decoration write, and, on a source-less grid, to the WG baseline too.
    pub(crate) fn seed_id(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        let (lx, lz) = self.to_local_exact(x, z);
        if self.in_bounds_local(lx, lz) && y >= self.min_y && y < self.min_y + self.height {
            self.blocks.insert_in_bounds((lx, y, lz), state);
            self.update_live_heights(lx, y, lz, state);
            if self.sources.is_none() {
                // Seeding after a probe must invalidate the WG lanes; ordinary
                // decoration writes do not, because those lanes are immutable
                // for the pass.
                let (local_lo, local_hi, min_y, height) =
                    (self.local_lo, self.local_hi, self.min_y, self.height);
                self.seeded_baseline
                    .get_or_insert_with(|| Overlay::with_bounds(local_lo, local_hi, min_y, height))
                    .insert_in_bounds((lx, y, lz), state);
                let cache_index = self.height_cache_index(lx, lz);
                let cache = self.height_cache[cache_index].get_mut();
                cache[1] = HEIGHT_CACHE_UNSET;
                cache[4] = HEIGHT_CACHE_UNSET;
            }
        }
    }
}

impl VegGrid<'_> {
    /// The state the source world holds at **local** `(lx, y, lz)`, or
    /// [`StateId::AIR`] on a source-less fixture.
    fn source_id(&self, lx: i32, y: i32, lz: i32) -> StateId {
        self.source().map_or(StateId::AIR, |source| {
            source.get_id(self.origin_x + lx, y, self.origin_z + lz)
        })
    }

    #[inline]
    fn source(&self) -> Option<&dyn StructureWorld> {
        self.sources.as_deref()
    }

    /// Positions written by `set_id_if_in_bounds` since construction, in write
    /// order, **as absolute world coordinates**, each paired with the state
    /// currently at that position (i.e. the *final* state if the same cell
    /// was written more than once, not an intermediate one) — what a caller
    /// should fold back into a wider grid, with no further translation
    /// needed.
    /// Positions written by `set_id_if_in_bounds`, with their local state ids.
    pub(crate) fn dirty_cells(&self) -> impl Iterator<Item = (i32, i32, i32, StateId)> {
        self.dirty.iter().map(|&(lx, y, lz)| {
            (
                self.origin_x + lx,
                y,
                self.origin_z + lz,
                self.blocks
                    .get_in_bounds(&(lx, y, lz))
                    .unwrap_or(StateId::AIR),
            )
        })
    }

    /// The number of writes recorded so far — a caller (currently only the
    /// tree placer) that brackets a `dirty_len()` call before and after a
    /// span of writes and then reads `dirty_cells().skip(before)` gets
    /// exactly the absolute-coordinate positions written in that span, in
    /// order. Used to compute one tree's own `trunks ∪ foliage ∪
    /// decorations` bounding box for leaf-distance propagation — see that
    /// function's own doc comment for why the bound matters.
    #[must_use]
    pub(crate) fn dirty_len(&self) -> usize {
        self.dirty.len()
    }

    fn in_bounds_local(&self, lx: i32, lz: i32) -> bool {
        (self.local_lo..self.local_hi).contains(&lx) && (self.local_lo..self.local_hi).contains(&lz)
    }

    #[inline]
    fn height_cache_index(&self, lx: i32, lz: i32) -> usize {
        debug_assert!(self.in_bounds_local(lx, lz));
        ((lz - self.local_lo) as usize) * self.local_width + (lx - self.local_lo) as usize
    }

    #[inline]
    fn update_live_heights(&self, lx: i32, y: i32, lz: i32, state: StateId) {
        let index = self.height_cache_index(lx, lz);
        let mut cache = self.height_cache[index].get();
        let mut facts = None;
        let mut changed = false;
        for lane in [0, 2, 3] {
            let height = cache[lane];
            if height == HEIGHT_CACHE_UNSET || y < height - 1 {
                continue;
            }
            let occupied = match lane {
                0 => !self.is_air_id(state),
                2 => facts.get_or_insert_with(|| base_facts(state)).is_motion_blocking(),
                3 => facts.get_or_insert_with(|| base_facts(state)).is_ocean_floor(),
                _ => unreachable!(),
            };
            let next = if occupied {
                height.max(y + 1)
            } else if y == height - 1 {
                HEIGHT_CACHE_UNSET
            } else {
                height
            };
            changed |= next != height;
            cache[lane] = next;
        }
        if changed {
            self.height_cache[index].set(cache);
        }
    }

    /// Absolute world `(x, z)` -> local `[local_lo, local_hi)`, **clamped**
    /// into range — used only by read paths, which must always answer
    /// something.
    fn to_local_clamped(&self, x: i32, z: i32) -> (i32, i32) {
        (
            (x - self.origin_x).clamp(self.local_lo, self.local_hi - 1),
            (z - self.origin_z).clamp(self.local_lo, self.local_hi - 1),
        )
    }

    /// Absolute world `(x, z)` -> local, **unclamped** — used only by the
    /// write path, which must know whether the position genuinely falls
    /// inside this chunk's own footprint rather than silently relocating a
    /// write to the nearest edge.
    fn to_local_exact(&self, x: i32, z: i32) -> (i32, i32) {
        (x - self.origin_x, z - self.origin_z)
    }

    /// This pass's own write if there is one, else the source chunk that owns the
    /// column, else air.
    ///
    /// Overlay-first is load-bearing, not an optimisation: vanilla's heightmaps
    /// update as decoration places blocks, so a tree placed earlier in the step
    /// must be visible to a later `height_world_surface` probe in the same step.
    /// A post-pass merge of the writes would answer stale and is parity-unsafe.
    fn get_local_id(&self, lx: i32, y: i32, lz: i32) -> StateId {
        if y < self.min_y || y >= self.min_y + self.height {
            return StateId::AIR;
        }
        match self.blocks.get_in_bounds(&(lx, y, lz)) {
            Some(id) => id,
            None => self.source_id(lx, y, lz),
        }
    }

    /// Interned read state. Kept as an explicit id-named alias for call sites
    /// that document their numeric hot path.
    #[must_use]
    pub(crate) fn get_id(&self, x: i32, y: i32, z: i32) -> StateId {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.get_local_id(lx, y, lz)
    }

    /// Writes past the local footprint or outside the vertical build range are
    /// dropped, not clamped: a clamped write would fabricate a block on the
    /// wrong column. Returns whether the write landed.
    pub(crate) fn set_id_if_in_bounds(&mut self, x: i32, y: i32, z: i32, state: StateId) -> bool {
        let (lx, lz) = self.to_local_exact(x, z);
        if self.in_bounds_local(lx, lz) && y >= self.min_y && y < self.min_y + self.height {
            let key = (lx, y, lz);
            self.blocks.insert_in_bounds(key, state);
            self.update_live_heights(lx, y, lz, state);
            self.dirty.push((lx, y, lz));
            true
        } else {
            false
        }
    }

    /// Whether `id` is one of the three air states.
    ///
    /// # Why this can be an id comparison and the fluid test below cannot
    ///
    /// **Air carries no block-state properties**, so for an air state
    /// `base_id(name) == name` and "base is one of three names" is exactly "id is
    /// one of three ids" — the three resolved when the grid is built.
    /// `crate::feature::vegetation::config::is_air` is still the definition; this
    /// is that definition pushed through the canonical table once per grid instead
    /// of once per cell. Adding a property-carrying state to `is_air` would silently
    /// break this, which is why that function's doc says not to.
    ///
    /// A fluid, by contrast, really does carry properties here —
    /// a generated column holds `minecraft:water[level=0]` — so the heightmap
    /// scans use the typed facts cached with each source palette entry rather
    /// than reducing a state to its base-name string.
    pub(super) fn is_air_id(&self, id: StateId) -> bool {
        self.air_ids.contains(&id)
    }
}

