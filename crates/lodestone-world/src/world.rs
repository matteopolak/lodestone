//! A [`ChunkPos`]-keyed store of loaded chunks.
//!
//! Chunk streaming is the allocation pattern that matters most: as the player
//! moves, columns at the trailing edge unload while new ones load at the
//! leading edge, so the same handful of packed-array size classes churn
//! continuously. [`World`] is deliberately a thin owner over [`LoadedChunk`]s so
//! that a future size-classed free pool can be dropped in without changing any
//! public signature: unloading a chunk can hand its packed `Vec<u64>` buffers to
//! the pool, and loading can take them back through
//! [`PackedArray::from_longs`](crate::PackedArray::from_longs), which every
//! decode path already routes through.

use std::collections::HashMap;
use std::collections::hash_map::{Iter, Values};
use std::sync::Arc;

use lodestone_core::Nbt;

use crate::block_entity::BlockEntity;
use crate::column::ChunkColumn;
use crate::heightmap::Heightmaps;
use crate::light::{ColumnLight, LightData, NibbleArray, SectionLight};
use crate::section::ChunkSection;

/// A chunk column's grid position (in chunk units, so block `x >> 4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkPos {
    /// Chunk X (block X divided by 16).
    pub x: i32,
    /// Chunk Z (block Z divided by 16).
    pub z: i32,
}

impl ChunkPos {
    /// Creates a chunk position from chunk-grid coordinates.
    #[must_use]
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// The chunk position containing a block at world `(block_x, block_z)`.
    #[must_use]
    pub const fn from_block(block_x: i32, block_z: i32) -> Self {
        Self {
            x: block_x >> 4,
            z: block_z >> 4,
        }
    }
}

/// Everything decoded from a single chunk packet: blocks and biomes, light,
/// heightmaps, and block entities.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedChunk {
    /// Block-state and biome sections.
    pub column: ChunkColumn,
    /// Sky and block light.
    pub light: ColumnLight,
    /// Column heightmaps.
    pub heightmaps: Heightmaps,
    /// Block entities within the column.
    pub block_entities: Vec<BlockEntity>,
}

impl LoadedChunk {
    /// Bundles the parts of a decoded chunk into one record.
    #[must_use]
    pub fn new(
        column: ChunkColumn,
        light: ColumnLight,
        heightmaps: Heightmaps,
        block_entities: Vec<BlockEntity>,
    ) -> Self {
        Self {
            column,
            light,
            heightmaps,
            block_entities,
        }
    }

    /// Total heap bytes owned by this chunk's storage.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.column.heap_bytes()
            + self.light.heap_bytes()
            + self.heightmaps.heap_bytes()
            + self.block_entities.capacity() * size_of::<BlockEntity>()
    }
}

/// A sparse, section-addressed update to a single column, applied via
/// [`World::merge`] / [`WorldSink::merge`].
///
/// Only the sections it names are applied; every other section of an existing
/// column is left exactly as it was. This is how partial chunk packets (1.8
/// `map_chunk` with `ground_up = false`, where absent sections mean *unchanged*)
/// apply without clobbering the sections they omit.
///
/// A version crate builds one by starting from an all-air `base` column of the
/// right shape (min-Y, section count, palette kinds, air/biome ids — the same it
/// would build for a full [`load`](World::load)) and pushing each decoded section
/// with [`set_section`](ColumnPatch::set_section). The `base` is used only when
/// the target column is not yet loaded, to create a partial column; when the
/// column already exists it is ignored (and is cheap regardless — an all-air
/// column allocates no per-section storage).
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnPatch {
    base: ChunkColumn,
    sections: Vec<(usize, ChunkSection)>,
}

impl ColumnPatch {
    /// Creates an empty patch whose `base` is the column shape used only if the
    /// target position is not yet loaded.
    #[must_use]
    pub fn new(base: ChunkColumn) -> Self {
        Self {
            base,
            sections: Vec::new(),
        }
    }

    /// Adds a section to apply at `section_index`. Overwrites any section already
    /// queued at the same index.
    pub fn set_section(&mut self, section_index: usize, section: ChunkSection) {
        if let Some(slot) = self.sections.iter_mut().find(|(i, _)| *i == section_index) {
            slot.1 = section;
        } else {
            self.sections.push((section_index, section));
        }
    }

    /// The number of sections queued in this patch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sections.len()
    }

    /// Whether the patch names no sections.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }
}

/// A sparse set of per-section light overwrites applied to an already-loaded
/// column by [`World::merge_light`].
///
/// This is the standalone-light-update seam. A `light_update` packet relights an
/// already-loaded column without changing any block, so it can't ride the block
/// [`merge`](World::merge) (whose [`ColumnPatch`] carries no light) nor
/// [`load`](World::load) (which would rebuild the whole column). Each entry is
/// `(light_section_index, LightData)`, using [`ColumnLight`]'s light-section
/// indexing (`0` is the section below the world, so light section `i` covers
/// world block-section `i - 1`) — the version crate that decodes the packet
/// already works in that space. Sky and block light are named independently
/// because a relight can touch one without the other.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LightPatch {
    sky: Vec<(usize, LightData)>,
    block: Vec<(usize, LightData)>,
}

impl LightPatch {
    /// Creates an empty light patch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues sky light for `light_section_index`, overwriting any already queued
    /// at that index.
    pub fn set_sky(&mut self, light_section_index: usize, data: LightData) {
        if let Some(slot) = self.sky.iter_mut().find(|(i, _)| *i == light_section_index) {
            slot.1 = data;
        } else {
            self.sky.push((light_section_index, data));
        }
    }

    /// Queues block light for `light_section_index`, overwriting any already
    /// queued at that index.
    pub fn set_block(&mut self, light_section_index: usize, data: LightData) {
        if let Some(slot) = self
            .block
            .iter_mut()
            .find(|(i, _)| *i == light_section_index)
        {
            slot.1 = data;
        } else {
            self.block.push((light_section_index, data));
        }
    }

    /// Total number of queued sky and block light sections.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sky.len() + self.block.len()
    }

    /// Whether the patch names no light sections.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sky.is_empty() && self.block.is_empty()
    }

    /// Builds a light patch from the wire fields of a modern `light_update`
    /// packet (protocol ≥ 1.14, where light travels separately from the chunk).
    ///
    /// This is the version-free half of `light_update` decoding: a version crate
    /// reads the four wire bitsets as `long[]` (LSB-first, exactly
    /// `BitSet.toLongArray()`) and the two lists of 2048-byte nibble arrays as
    /// [`NibbleArray`]s (via [`NibbleArray::from_bytes`], in wire order), then
    /// hands them here. All three-state light semantics live in this one tested
    /// place so no adapter has to re-derive them:
    ///
    /// * bit `i` of `sky_mask`/`block_mask` set ⇒ that light section carries a
    ///   full array, taken from `sky_arrays`/`block_arrays` **in ascending
    ///   section order** ⇒ [`LightData::Values`].
    /// * bit `i` of `empty_sky_mask`/`empty_block_mask` set ⇒ that section is
    ///   *explicitly all-zero* ⇒ [`LightData::Uniform(0)`](LightData::Uniform).
    ///   This is the correctness trap: an "empty" light section is **not** the
    ///   same as an absent one — it means zero light, so it must overwrite any
    ///   prior value, not be skipped.
    /// * a section named by neither mask is left out of the patch entirely, so
    ///   [`merge_light`](World::merge_light) leaves its light unchanged.
    ///
    /// Section indices are light-section indices (`0` is the boundary section
    /// below the world), matching both the packet's own ordering and
    /// [`ColumnLight`]'s indexing, so no offset is applied. Should a server ever
    /// set both a section's full-mask and empty-mask bit (vanilla never does —
    /// they are mutually exclusive), the full array wins.
    #[must_use]
    pub fn from_light_masks(
        sky_mask: &[u64],
        empty_sky_mask: &[u64],
        sky_arrays: Vec<NibbleArray>,
        block_mask: &[u64],
        empty_block_mask: &[u64],
        block_arrays: Vec<NibbleArray>,
    ) -> Self {
        Self {
            sky: light_layer_from_masks(sky_mask, empty_sky_mask, sky_arrays),
            block: light_layer_from_masks(block_mask, empty_block_mask, block_arrays),
        }
    }
}

/// Tests bit `i` of a wire bitset stored as an LSB-first `long[]`.
fn mask_bit(mask: &[u64], i: usize) -> bool {
    mask.get(i >> 6)
        .is_some_and(|word| (word >> (i & 63)) & 1 == 1)
}

/// Returns one past the highest set bit of an LSB-first `long[]` bitset, i.e. the
/// exclusive upper bound of section indices it can name.
fn mask_bit_len(mask: &[u64]) -> usize {
    for (word_index, &word) in mask.iter().enumerate().rev() {
        if word != 0 {
            return (word_index << 6) + (64 - word.leading_zeros() as usize);
        }
    }
    0
}

/// Resolves one light layer's `(index, LightData)` entries from its full-array
/// mask, empty mask and in-order array list. See [`LightPatch::from_light_masks`]
/// for the semantics this encodes.
fn light_layer_from_masks(
    mask: &[u64],
    empty_mask: &[u64],
    arrays: Vec<NibbleArray>,
) -> Vec<(usize, LightData)> {
    let mut out = Vec::new();
    let mut arrays = arrays.into_iter();
    let bound = mask_bit_len(mask).max(mask_bit_len(empty_mask));
    for i in 0..bound {
        if mask_bit(mask, i) {
            if let Some(array) = arrays.next() {
                out.push((i, LightData::Values(array)));
            }
        } else if mask_bit(empty_mask, i) {
            out.push((i, LightData::Uniform(0)));
        }
    }
    out
}

/// A store of loaded chunk columns keyed by [`ChunkPos`].
///
/// Chunks are stored inline (not behind a per-chunk `Arc`), because copy-on-write
/// lives one level down: each [`ChunkColumn`] holds its sections as
/// `Arc<ChunkSection>`. That granularity is a hard requirement of two seams at
/// once:
///
/// * **Meshing** holds a 27-section neighbourhood for the whole (slow) mesh. It
///   pulls those sections through [`section`](World::section), which hands back an
///   owned `Arc<ChunkSection>` carrying no borrow into the world and pinning no
///   lock — the mesher grabs its 27 neighbours, drops the lock, and meshes while
///   chunk streaming proceeds. The clone is a refcount bump, never a copy of
///   section data.
/// * **Block updates** touch one block, so [`get_mut`](World::get_mut) mutating a
///   single section forks *only that section* (and only while a mesher holds it),
///   never the whole column. A per-block edit is bounded by one small section,
///   not proportional to a 24-section column — the constraint that a block update
///   must never rebuild a column.
///
/// [`get`](World::get) returns a plain borrow for cheap in-process reads (a bot's
/// `block_at`); the lock-free, own-it-after-unlock path is the section snapshot.
/// A stale snapshot is fine by design: the next `ChunkLoaded { pos }` re-dirties
/// the region and it re-meshes, so no consistency protocol is needed.
///
/// [`ChunkColumn`]: crate::ChunkColumn
#[derive(Debug, Clone, Default)]
pub struct World {
    chunks: HashMap<ChunkPos, LoadedChunk>,
}

impl World {
    /// Creates an empty world.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads (inserts or replaces) the chunk at `pos`, returning any previous
    /// occupant. A future pool would reclaim a returned chunk's buffers here.
    pub fn load(&mut self, pos: ChunkPos, chunk: LoadedChunk) -> Option<LoadedChunk> {
        self.chunks.insert(pos, chunk)
    }

    /// Unloads and returns the chunk at `pos`, if present. A size-classed pool
    /// could recycle the returned chunk's packed buffers.
    pub fn unload(&mut self, pos: ChunkPos) -> Option<LoadedChunk> {
        self.chunks.remove(&pos)
    }

    /// Applies a sparse [`ColumnPatch`] to the chunk at `pos`, replacing only the
    /// sections the patch names and leaving every other section — plus the
    /// column's light, heightmaps, and block entities — untouched.
    ///
    /// This is the partial-application seam: 1.8 `map_chunk` with
    /// `ground_up = false` carries a subset of sections where absent sections mean
    /// *unchanged*, so applying it through [`load`](World::load) (wholesale
    /// replace) would clobber the omitted sections. `merge` is also the natural
    /// home for later per-section updates. `load` keeps wholesale-replace
    /// semantics for full columns; `merge` is strictly additive/overlaying.
    ///
    /// If no column is loaded at `pos`, the patch's `base` skeleton is used to
    /// create a fresh (partial) column before applying the named sections — a
    /// legal `ground_up = false` for an unseen column produces a partial column
    /// rather than being silently dropped.
    pub fn merge(&mut self, pos: ChunkPos, patch: ColumnPatch) {
        let ColumnPatch { base, sections } = patch;
        let chunk = self.chunks.entry(pos).or_insert_with(|| {
            let light = ColumnLight::new(base.section_count());
            LoadedChunk::new(base, light, Heightmaps::new(), Vec::new())
        });
        for (index, section) in sections {
            chunk.column.set_section(index, Some(section));
        }
    }

    /// Sets the block-state id at absolute world coordinates, mutating the one
    /// loaded section that owns it in place.
    ///
    /// This is the single-block update seam that `block_update` (one call) and
    /// `section_blocks_update` (one call per changed block) route into. It is a
    /// **no-op** — never a panic — if the owning chunk is not loaded or `y` falls
    /// outside the column's height range: block updates can legally arrive for a
    /// chunk we have not received or have already unloaded, and dropping those is
    /// correct rather than exceptional.
    ///
    /// The write forks only the one section it touches (copy-on-write via the
    /// column's `Arc<ChunkSection>`), so a mesher holding an older section
    /// snapshot keeps seeing the pre-edit blocks and a per-block update never
    /// rebuilds a column. `x`/`z` are absolute block coordinates (routed to
    /// `x >> 4`, `z >> 4`); `y` may be negative (overworld `min_y = -64`).
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, state: u32) {
        let pos = ChunkPos::from_block(x, z);
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return;
        };
        // Guard the out-of-column case ourselves: ChunkColumn::set_block panics
        // for a `y` outside its height, but a stray update must be ignored.
        if chunk.column.section_index(y).is_none() {
            return;
        }
        let sx = (x & 15) as usize;
        let sz = (z & 15) as usize;
        chunk.column.set_block(sx, y, sz, state);
    }

    /// Applies many block writes that all fall within a **single** section,
    /// keyed by that section's grid coordinates, forking the section's `Arc` at
    /// most once for the whole batch.
    ///
    /// This is the `section_blocks_update` seam. `section_x`/`section_z` are the
    /// chunk's grid coordinates (block `>> 4`); `section_y` is the **absolute**
    /// section index (`floor(world_y / 16)`, e.g. `-4` for the bottom section of
    /// a `min_y = -64` column). Each `blocks` entry is `(local_x, local_y,
    /// local_z, state)` with section-relative coordinates in `0..16`.
    ///
    /// Like [`set_block`](World::set_block) it is a **no-op** — never a panic —
    /// if the chunk is not loaded or the section index is outside the column, and
    /// it never touches light, so the version crate stays free of the wire
    /// packing (which it unpacks before calling).
    pub fn set_blocks(
        &mut self,
        section_x: i32,
        section_y: i32,
        section_z: i32,
        blocks: &[(u8, u8, u8, u32)],
    ) {
        let pos = ChunkPos::new(section_x, section_z);
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return;
        };
        // Absolute section index → column-relative index; ignore out-of-column.
        let bottom_section = chunk.column.min_y() >> 4;
        let Ok(index) = usize::try_from(section_y - bottom_section) else {
            return;
        };
        chunk.column.set_blocks_in_section(index, blocks);
    }

    /// Inserts or replaces a single block entity's type and NBT payload at
    /// absolute world coordinates, matching the `block_entity_data` packet.
    ///
    /// This is the `block_entity_data` seam: unlike the bulk list a chunk
    /// packet carries at load time, this packet republishes exactly one block
    /// entity, keyed by its position. If an entry already exists at
    /// `(x, y, z)` its type and NBT are replaced in place; otherwise a new
    /// entry is appended. A **no-op** — never a panic — when the owning chunk
    /// is not loaded, for the same reason as [`set_block`](World::set_block):
    /// updates can arrive for chunks we do not hold.
    pub fn set_block_entity(&mut self, x: i32, y: i32, z: i32, type_id: u32, nbt: Nbt) {
        let pos = ChunkPos::from_block(x, z);
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return;
        };
        let rel_x = (x & 15) as u8;
        let rel_z = (z & 15) as u8;
        let y = y as i16;
        if let Some(existing) = chunk
            .block_entities
            .iter_mut()
            .find(|be| be.rel_x == rel_x && be.rel_z == rel_z && be.y == y)
        {
            existing.type_id = type_id;
            existing.nbt = nbt;
        } else {
            chunk.block_entities.push(BlockEntity {
                rel_x,
                rel_z,
                y,
                type_id,
                nbt,
            });
        }
    }

    /// Applies a sparse [`LightPatch`] to the chunk at `pos`, overwriting only the
    /// light sections it names and leaving blocks, heightmaps, and unnamed light
    /// sections untouched.
    ///
    /// This is the `light_update` seam. It is deliberately a **no-op** when the
    /// chunk is not loaded: a standalone relight for a column we do not hold is
    /// redundant, because the `level_chunk_with_light` packet that eventually
    /// loads the column carries its own light. Unlike block [`merge`](World::merge)
    /// this does not synthesise an absent column — light alone cannot define a
    /// column's shape (min-Y, section count, palettes), and inventing a skeleton
    /// from it would be the fictional-data failure this project keeps catching.
    ///
    /// The world **stores** light; it never **computes** it. `set_block` leaves
    /// light untouched precisely because recomputation belongs to the authority
    /// (the real server, or the in-process singleplayer server), which relights
    /// and pushes `light_update`; the client applies it here. See the crate-level
    /// note on why no lighting engine lives in this version-free storage crate.
    pub fn merge_light(&mut self, pos: ChunkPos, patch: LightPatch) {
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return;
        };
        let count = chunk.light.light_section_count();
        for (i, data) in patch.sky {
            if i < count {
                *chunk.light.sky_mut(i) = data;
            }
        }
        for (i, data) in patch.block {
            if i < count {
                *chunk.light.block_mut(i) = data;
            }
        }
    }

    /// Whether a chunk is loaded at `pos`.
    #[must_use]
    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.chunks.contains_key(&pos)
    }

    /// Borrows the chunk at `pos` for a cheap in-process read.
    ///
    /// The borrow is tied to the world, so it must not be held across a mesh — for
    /// that, pull owned section snapshots with [`section`](World::section), which
    /// carry no borrow and outlive any world lock. This borrow form is for callers
    /// that read and release immediately, such as a bot's `block_at`.
    #[must_use]
    pub fn get(&self, pos: ChunkPos) -> Option<&LoadedChunk> {
        self.chunks.get(&pos)
    }

    /// Returns an owned, lock-free clone of the `Arc<ChunkSection>` at
    /// `section_index` within the chunk at `pos`, if both are present.
    ///
    /// This is the mesher's read seam: it bumps a section refcount rather than
    /// copying, so the caller can drop any world lock and mesh off a stable
    /// snapshot. A later edit of that section forks it copy-on-write, leaving the
    /// snapshot untouched. `None` means either the chunk is not loaded or the
    /// section is elided (all air).
    #[must_use]
    pub fn section(&self, pos: ChunkPos, section_index: usize) -> Option<Arc<ChunkSection>> {
        self.chunks.get(&pos)?.column.section_arc(section_index)
    }

    /// Returns an O(1), lock-free light snapshot of light section
    /// `light_section_index` within the chunk at `pos`, if the chunk is loaded
    /// and that light section exists.
    ///
    /// This is the light-side companion to [`section`](World::section): the
    /// mesher reads block state through one and light through the other from the
    /// same `&World` borrow, so the two are consistent. Light is indexed in its
    /// native light-section space (`0` is the section below the world; light
    /// section `i` covers world block-section `i - 1`), which is what lets the
    /// mesher reach the boundary light sections above and below the build range
    /// that a section at the top or bottom of a column samples into.
    ///
    /// Unlike [`section`](World::section) this does **not** return `None` for an
    /// all-air (elided) block section: air carries light, and a face meshed
    /// against it must sample that light. Only an unloaded chunk or an
    /// out-of-range light section yields `None`.
    #[must_use]
    pub fn section_light(&self, pos: ChunkPos, light_section_index: usize) -> Option<SectionLight> {
        let chunk = self.chunks.get(&pos)?;
        if light_section_index >= chunk.light.light_section_count() {
            return None;
        }
        Some(chunk.light.section_light(light_section_index))
    }

    /// Mutably borrows the chunk at `pos`.
    ///
    /// The chunk itself is owned inline, so this is a plain borrow. Copy-on-write
    /// happens beneath it at section granularity: a block edit through the
    /// returned column forks only the one section a reader holds, never the whole
    /// column.
    pub fn get_mut(&mut self, pos: ChunkPos) -> Option<&mut LoadedChunk> {
        self.chunks.get_mut(&pos)
    }

    /// Number of loaded chunks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Whether no chunks are loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Iterates loaded chunks.
    pub fn values(&self) -> Values<'_, ChunkPos, LoadedChunk> {
        self.chunks.values()
    }

    /// Iterates `(pos, chunk)` pairs.
    pub fn iter(&self) -> Iter<'_, ChunkPos, LoadedChunk> {
        self.chunks.iter()
    }

    /// Total heap bytes owned by every loaded chunk (excludes the map's own
    /// bucket array).
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.chunks.values().map(LoadedChunk::heap_bytes).sum()
    }
}

/// A write-only destination a version adapter applies decoded chunks to as it
/// processes chunk packets.
///
/// The client owns the concrete world behind this trait and decides how readers
/// observe it — a shared lock, a periodic snapshot, or an `arc-swap` of an
/// immutable world. The adapter never needs to know: it only calls
/// [`load`](WorldSink::load) and [`unload`](WorldSink::unload). Keeping the seam
/// a trait (rather than a concrete `&mut World`) is deliberate — it lets the
/// client change its read/query strategy without another change to the adapter
/// signature, which crosses every version crate.
///
/// Chunk data flows through this sink and **not** through the event stream, so
/// world state can never become reconstructible only from a bounded or lossy
/// channel: a slow event consumer cannot stall packet processing, and a
/// late-attaching consumer still sees every chunk by querying the world.
pub trait WorldSink {
    /// Loads (inserts or replaces) the chunk at `pos`. Any previous occupant is
    /// dropped; a caller that needs it can use [`World::load`] directly.
    fn load(&mut self, pos: ChunkPos, chunk: LoadedChunk);

    /// Applies a sparse [`ColumnPatch`] to the chunk at `pos`, replacing only the
    /// sections it names and leaving the rest of the column untouched.
    ///
    /// This is a first-class sink operation, not a convenience over
    /// [`load`](WorldSink::load): partial chunk packets (1.8 `ground_up = false`,
    /// later per-section updates) carry a subset of sections, and applying them
    /// through `load` would clobber the sections they omit. It is a required
    /// method so no sink can silently drop a partial update — the same failure
    /// class this seam exists to prevent.
    fn merge(&mut self, pos: ChunkPos, patch: ColumnPatch);

    /// Sets the block-state id at absolute world coordinates, mutating the single
    /// loaded section that owns it in place.
    ///
    /// This is the single-block update seam `block_update` and
    /// `section_blocks_update` route into. Coordinates are absolute (`x`/`z` are
    /// routed to their chunk via `>> 4`; `y` may be negative). It must be a
    /// **no-op**, never a panic, when the owning chunk is not loaded or `y` is
    /// outside the column — updates can arrive for chunks we do not hold. Like
    /// [`merge`](WorldSink::merge) it is required, so no sink drops updates by
    /// inheriting a silent default.
    fn set_block(&mut self, x: i32, y: i32, z: i32, state: u32);

    /// Applies many block writes that all fall within one section, keyed by that
    /// section's grid coordinates, forking the section's storage at most once.
    ///
    /// This is the bulk seam for `section_blocks_update`, whose packet carries
    /// many positions in a single section. `section_y` is the **absolute**
    /// section index; `blocks` entries are section-relative `(x, y, z, state)`.
    /// No-op if the chunk or section is not present. Required for the same reason
    /// as [`set_block`](WorldSink::set_block).
    fn set_blocks(
        &mut self,
        section_x: i32,
        section_y: i32,
        section_z: i32,
        blocks: &[(u8, u8, u8, u32)],
    );

    /// Inserts or replaces a single block entity's type and NBT payload at
    /// absolute world coordinates.
    ///
    /// This is the `block_entity_data` seam. Like [`set_block`](WorldSink::set_block)
    /// it is a **no-op**, never a panic, when the owning chunk is not loaded, and
    /// it is required for the same reason: a silent default would drop the
    /// update rather than surfacing the gap.
    fn set_block_entity(&mut self, x: i32, y: i32, z: i32, type_id: u32, nbt: Nbt);

    /// Applies a sparse [`LightPatch`] to the chunk at `pos`, overwriting only the
    /// light sections it names.
    ///
    /// This is the `light_update` seam. It is a **no-op** when the chunk is not
    /// loaded (the chunk packet carries its own light) and never touches blocks.
    /// Required for the same reason as [`merge`](WorldSink::merge): a silent
    /// default would drop relights and reintroduce the stale-light black-face
    /// trap this seam exists to close.
    fn merge_light(&mut self, pos: ChunkPos, patch: LightPatch);

    /// Unloads the chunk at `pos`, if present.
    fn unload(&mut self, pos: ChunkPos);
}

impl WorldSink for World {
    fn load(&mut self, pos: ChunkPos, chunk: LoadedChunk) {
        World::load(self, pos, chunk);
    }

    fn merge(&mut self, pos: ChunkPos, patch: ColumnPatch) {
        World::merge(self, pos, patch);
    }

    fn set_block(&mut self, x: i32, y: i32, z: i32, state: u32) {
        World::set_block(self, x, y, z, state);
    }

    fn set_blocks(
        &mut self,
        section_x: i32,
        section_y: i32,
        section_z: i32,
        blocks: &[(u8, u8, u8, u32)],
    ) {
        World::set_blocks(self, section_x, section_y, section_z, blocks);
    }

    fn set_block_entity(&mut self, x: i32, y: i32, z: i32, type_id: u32, nbt: Nbt) {
        World::set_block_entity(self, x, y, z, type_id, nbt);
    }

    fn merge_light(&mut self, pos: ChunkPos, patch: LightPatch) {
        World::merge_light(self, pos, patch);
    }

    fn unload(&mut self, pos: ChunkPos) {
        World::unload(self, pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Heightmap, LightData, NibbleArray, PaletteKind};
    use std::sync::Arc;

    fn sample_chunk() -> LoadedChunk {
        let column = ChunkColumn::new(
            -64,
            24,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            0,
            0,
        );
        let light = ColumnLight::new(24);
        let mut heightmaps = Heightmaps::new();
        heightmaps.insert(0, Heightmap::new(384));
        LoadedChunk::new(column, light, heightmaps, Vec::new())
    }

    fn empty_modern_column() -> ChunkColumn {
        ChunkColumn::new(
            -64,
            24,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            0,
            0,
        )
    }

    fn section_with_block(x: usize, ly: usize, z: usize, value: u32) -> ChunkSection {
        let mut s = ChunkSection::new(PaletteKind::block_states(), PaletteKind::biomes(), 0, 0);
        s.set_block(x, ly, z, value);
        s
    }

    #[test]
    fn from_block_maps_to_chunk_grid() {
        assert_eq!(ChunkPos::from_block(0, 0), ChunkPos::new(0, 0));
        assert_eq!(ChunkPos::from_block(31, 16), ChunkPos::new(1, 1));
        assert_eq!(ChunkPos::from_block(-1, -16), ChunkPos::new(-1, -1));
    }

    #[test]
    fn load_unload_round_trip() {
        let mut world = World::new();
        let pos = ChunkPos::new(2, -3);
        assert!(!world.contains(pos));
        assert!(world.is_empty());

        assert!(world.load(pos, sample_chunk()).is_none());
        assert!(world.contains(pos));
        assert_eq!(world.len(), 1);
        assert!(world.get(pos).is_some());

        let removed = world.unload(pos).expect("chunk present");
        assert_eq!(removed.column.section_count(), 24);
        assert!(!world.contains(pos));
        assert!(world.is_empty());
    }

    #[test]
    fn load_replaces_and_returns_previous() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());
        let previous = world.load(pos, sample_chunk());
        assert!(previous.is_some(), "reload should return the old chunk");
        assert_eq!(world.len(), 1);
    }

    #[test]
    fn heap_bytes_sums_loaded_chunks() {
        let mut world = World::new();
        world.load(ChunkPos::new(0, 0), sample_chunk());
        world.load(ChunkPos::new(1, 0), sample_chunk());
        assert!(world.heap_bytes() > 0);
    }

    #[test]
    fn section_hands_out_a_shared_arc_not_a_copy() {
        // Meshing grabs section snapshots and holds them for the whole (slow)
        // mesh. Each `section` must be a refcount bump onto the *same* allocation,
        // never a deep copy of section data.
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());
        world
            .get_mut(pos)
            .expect("present")
            .column
            .set_block(0, -64, 0, 1);
        let a = world.section(pos, 0).expect("section present");
        let b = world.section(pos, 0).expect("section present");
        assert!(
            Arc::ptr_eq(&a, &b),
            "two snapshots share one allocation (no clone of section data)"
        );
    }

    #[test]
    fn held_section_outlives_unload() {
        // A mesher that took a section keeps reading it even as streaming unloads
        // the chunk. The returned `Arc` is owned, so it holds no borrow into the
        // world and pins no lock — chunk loading proceeds behind an in-flight mesh.
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());
        world
            .get_mut(pos)
            .expect("present")
            .column
            .set_block(0, -64, 0, 5);
        let held = world.section(pos, 0).expect("section present");
        world.unload(pos);
        assert!(!world.contains(pos));
        assert_eq!(held.get_block(0, 0, 0), 5, "held section stays readable");
    }

    #[test]
    fn block_edit_is_copy_on_write_at_section_granularity() {
        // A block edit arriving while a mesher holds the section must not mutate
        // the reader's snapshot, and must cost one *section* clone at most — never
        // a column rebuild.
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());
        world
            .get_mut(pos)
            .expect("present")
            .column
            .set_block(0, -64, 0, 1);
        let held = world.section(pos, 0).expect("section present");
        world
            .get_mut(pos)
            .expect("present")
            .column
            .set_block(0, -64, 0, 9);
        assert_eq!(
            held.get_block(0, 0, 0),
            1,
            "reader's section is undisturbed"
        );
        assert_eq!(
            world.get(pos).expect("present").column.get_block(0, -64, 0),
            9,
            "world sees the edit"
        );
        let after = world.section(pos, 0).expect("section present");
        assert!(
            !Arc::ptr_eq(&held, &after),
            "edit forked a fresh section away from the live reader"
        );
    }

    #[test]
    fn block_edit_in_place_when_section_unshared() {
        // With no reader outstanding the common case must be a plain in-place
        // mutation: no copy, no reallocation of the section or the column.
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());
        world
            .get_mut(pos)
            .expect("present")
            .column
            .set_block(0, -64, 0, 1);
        let before = Arc::as_ptr(&world.section(pos, 0).expect("present"));
        world
            .get_mut(pos)
            .expect("present")
            .column
            .set_block(1, -64, 1, 7);
        let after = Arc::as_ptr(&world.section(pos, 0).expect("present"));
        assert_eq!(before, after, "no reader → mutate in place, no rebuild");
        assert_eq!(
            world.get(pos).expect("present").column.get_block(1, -64, 1),
            7
        );
    }

    // --- Sparse section merge (§12.51) ---
    //
    // Partial chunk packets (1.8 `map_chunk` with `ground_up = false`, and later
    // per-section updates) must apply only the sections they carry and leave
    // every other section untouched — never a wholesale `load` that clobbers the
    // sections the packet omitted.

    #[test]
    fn merge_overlays_only_named_sections_by_arc_identity() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);

        // A full column with four distinct populated sections.
        let mut chunk = sample_chunk();
        chunk.column.set_block(0, -64, 0, 1); // section 0
        chunk.column.set_block(0, -48, 0, 2); // section 1
        chunk.column.set_block(0, -32, 0, 3); // section 2
        chunk.column.set_block(0, -16, 0, 4); // section 3
        world.load(pos, chunk);

        let s0 = Arc::as_ptr(&world.section(pos, 0).expect("present"));
        let s1 = Arc::as_ptr(&world.section(pos, 1).expect("present"));
        let s3 = Arc::as_ptr(&world.section(pos, 3).expect("present"));

        // Merge a patch that names only section 2.
        let mut patch = ColumnPatch::new(empty_modern_column());
        patch.set_section(2, section_with_block(0, 0, 0, 99));
        world.merge(pos, patch);

        // Section 2 replaced wholesale; 0, 1, 3 are the same allocations.
        assert_eq!(world.get(pos).unwrap().column.get_block(0, -32, 0), 99);
        assert_eq!(world.get(pos).unwrap().column.get_block(0, -64, 0), 1);
        assert_eq!(
            Arc::as_ptr(&world.section(pos, 0).expect("present")),
            s0,
            "section 0 untouched"
        );
        assert_eq!(
            Arc::as_ptr(&world.section(pos, 1).expect("present")),
            s1,
            "section 1 untouched"
        );
        assert_eq!(
            Arc::as_ptr(&world.section(pos, 3).expect("present")),
            s3,
            "section 3 untouched"
        );
    }

    #[test]
    fn merge_replaces_a_present_section_wholesale_not_per_block() {
        // A merged section replaces the *whole* section, not a per-block union:
        // a block set in the old section but absent from the patch is gone.
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        let mut chunk = sample_chunk();
        chunk.column.set_block(0, -64, 0, 1);
        chunk.column.set_block(5, -64, 5, 2);
        world.load(pos, chunk);

        let mut patch = ColumnPatch::new(empty_modern_column());
        patch.set_section(0, section_with_block(0, 0, 0, 8));
        world.merge(pos, patch);

        assert_eq!(world.get(pos).unwrap().column.get_block(0, -64, 0), 8);
        assert_eq!(
            world.get(pos).unwrap().column.get_block(5, -64, 5),
            0,
            "old block not in the patch section is gone (wholesale replace)"
        );
    }

    #[test]
    fn merge_into_absent_column_creates_a_partial_column() {
        // A legal `ground_up = false` for a never-seen column must not be dropped
        // (world state must never depend on a lossy stream): it creates a partial
        // column carrying just the sections it names.
        let mut world = World::new();
        let pos = ChunkPos::new(5, -2);
        let mut patch = ColumnPatch::new(empty_modern_column());
        patch.set_section(5, section_with_block(1, 0, 1, 7));
        world.merge(pos, patch);

        assert!(world.contains(pos));
        // Section 5 of a min_y=-64 column starts at y = -64 + 5*16 = 16.
        assert_eq!(world.get(pos).unwrap().column.get_block(1, 16, 1), 7);
        assert!(world.section(pos, 0).is_none(), "unnamed sections stay air");
        assert!(world.section(pos, 5).is_some());
    }

    #[test]
    fn merge_leaves_light_and_heightmaps_untouched() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        let chunk = sample_chunk();
        let light_before = chunk.light.clone();
        let heightmaps_before = chunk.heightmaps.clone();
        world.load(pos, chunk);

        let mut patch = ColumnPatch::new(empty_modern_column());
        patch.set_section(1, section_with_block(0, 0, 0, 4));
        world.merge(pos, patch);

        assert_eq!(
            world.get(pos).unwrap().light,
            light_before,
            "merge must not disturb existing light"
        );
        assert_eq!(
            world.get(pos).unwrap().heightmaps,
            heightmaps_before,
            "merge must not disturb existing heightmaps"
        );
    }

    #[test]
    fn set_block_mutates_a_loaded_section_in_place() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());

        // Absolute (5, 70, 9) lives in chunk (0,0), section-relative (5, .., 9).
        world.set_block(5, 70, 9, 42);

        assert_eq!(
            world.get(pos).unwrap().column.get_block(5, 70, 9),
            42,
            "set_block must be visible through the world read path"
        );
        // Still exactly one chunk — a block edit must never reload the column.
        assert_eq!(world.len(), 1);
    }

    #[test]
    fn set_block_routes_absolute_coords_to_the_owning_chunk() {
        let mut world = World::new();
        // Negative coords exercise the >>4 routing and the &15 section-relative
        // mask, where naive `/16`/`%16` would land in the wrong chunk/cell.
        let pos = ChunkPos::new(-1, -1);
        world.load(pos, sample_chunk());

        // Block x=-1 → chunk x=-1, section-relative x=15; z=-3 → cell z=13.
        world.set_block(-1, 0, -3, 7);

        assert_eq!(
            world.get(pos).unwrap().column.get_block(15, 0, 13),
            7,
            "absolute (-1,0,-3) must route to chunk (-1,-1) cell (15,·,13)"
        );
        assert!(
            world.get(ChunkPos::new(0, 0)).is_none(),
            "the edit must not touch a neighbouring chunk"
        );
    }

    #[test]
    fn set_block_is_a_noop_for_an_unloaded_chunk() {
        let mut world = World::new();
        // No chunk loaded anywhere: a stray update must not panic or create one.
        world.set_block(100, 64, 100, 9);
        assert!(
            world.is_empty(),
            "set_block must not create an absent chunk"
        );
    }

    #[test]
    fn set_block_is_a_noop_for_y_outside_the_column() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());
        let before = world.get(pos).unwrap().column.clone();

        // min_y = -64, max_y = 320; both of these are out of range and must be
        // silently ignored rather than panicking through ChunkColumn::set_block.
        world.set_block(0, -65, 0, 5);
        world.set_block(0, 320, 0, 5);

        assert_eq!(
            &world.get(pos).unwrap().column,
            &before,
            "an out-of-column y must leave the column untouched"
        );
    }

    #[test]
    fn set_block_forks_only_the_held_section_copy_on_write() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        let mut chunk = sample_chunk();
        // Seed a block so the target section is allocated and snapshot-able.
        chunk.column.set_block(1, 70, 1, 11);
        world.load(pos, chunk);

        let section_index = world
            .get(pos)
            .unwrap()
            .column
            .section_index(70)
            .expect("y in range");
        // The mesher's snapshot: an owned Arc that must not observe later edits.
        let snapshot = world.section(pos, section_index).expect("section present");
        let snapshot_before = snapshot.get_block(1, 70i32.rem_euclid(16) as usize, 1);

        world.set_block(1, 70, 1, 99);

        assert_eq!(
            snapshot.get_block(1, 70i32.rem_euclid(16) as usize, 1),
            snapshot_before,
            "the held section snapshot must be frozen (copy-on-write)"
        );
        assert_eq!(
            world.get(pos).unwrap().column.get_block(1, 70, 1),
            99,
            "the world must observe the new value"
        );
    }

    #[test]
    fn set_block_is_reachable_through_the_worldsink_trait() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());

        let sink: &mut dyn WorldSink = &mut world;
        sink.set_block(2, 65, 3, 55);

        assert_eq!(
            world.get(pos).unwrap().column.get_block(2, 65, 3),
            55,
            "set_block must dispatch through &mut dyn WorldSink"
        );
    }

    /// A chunk with a distinct non-air block seeded in every one of its 24
    /// sections, so each section is allocated (not elided) and individually
    /// snapshot-able by `Arc` identity.
    fn chunk_with_every_section_present() -> LoadedChunk {
        let mut chunk = sample_chunk();
        let min_y = chunk.column.min_y();
        for s in 0..chunk.column.section_count() {
            let y = min_y + (s as i32) * 16 + 1;
            // A per-section distinct id keeps every section non-air.
            chunk.column.set_block(0, y, 0, (s as u32) + 1);
        }
        chunk
    }

    #[test]
    fn set_block_leaves_sibling_section_arcs_pointer_identical() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, chunk_with_every_section_present());
        let count = world.get(pos).unwrap().column.section_count();

        // Hold a snapshot of every section so make_mut on the touched one must
        // fork (refcount > 1), making the pointer change observable.
        let before: Vec<Arc<ChunkSection>> = (0..count)
            .map(|i| world.section(pos, i).expect("section present"))
            .collect();

        // Edit a single block in section index 5 (y = min_y + 5*16 + 2).
        let target = 5usize;
        let min_y = world.get(pos).unwrap().column.min_y();
        world.set_block(0, min_y + (target as i32) * 16 + 2, 0, 4242);

        for (i, prev) in before.iter().enumerate() {
            let now = world.section(pos, i).expect("section still present");
            if i == target {
                assert!(
                    !Arc::ptr_eq(prev, &now),
                    "the touched section must fork to a new Arc"
                );
            } else {
                assert!(
                    Arc::ptr_eq(prev, &now),
                    "sibling section {i} must keep a pointer-identical Arc — \
                     a block edit must not clone the column"
                );
            }
        }
    }

    #[test]
    fn set_blocks_forks_one_section_once_for_many_blocks() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, chunk_with_every_section_present());
        let count = world.get(pos).unwrap().column.section_count();
        let min_y = world.get(pos).unwrap().column.min_y();

        let before: Vec<Arc<ChunkSection>> = (0..count)
            .map(|i| world.section(pos, i).expect("present"))
            .collect();

        // Many block changes within ONE section (index 3). A naive per-block
        // loop would make_mut repeatedly; the bulk op must fork the section once.
        let target = 3usize;
        let section_y = (min_y >> 4) + target as i32;
        let blocks: Vec<(u8, u8, u8, u32)> = (0..16u8).map(|i| (i, 2, i, 500 + i as u32)).collect();
        world.set_blocks(0, section_y, 0, &blocks);

        // Every write landed.
        for i in 0..16u8 {
            assert_eq!(
                world.get(pos).unwrap().column.get_block(
                    i as usize,
                    min_y + target as i32 * 16 + 2,
                    i as usize
                ),
                500 + i as u32,
                "bulk write {i} must be visible"
            );
        }
        // Exactly one section forked; all siblings pointer-identical.
        for (i, prev) in before.iter().enumerate() {
            let now = world.section(pos, i).expect("present");
            if i == target {
                assert!(!Arc::ptr_eq(prev, &now), "target section forked once");
            } else {
                assert!(Arc::ptr_eq(prev, &now), "sibling {i} untouched");
            }
        }
    }

    #[test]
    fn set_blocks_routes_negative_section_coords() {
        let mut world = World::new();
        let pos = ChunkPos::new(-1, -1);
        world.load(pos, chunk_with_every_section_present());
        let min_y = world.get(pos).unwrap().column.min_y();

        // Bottom section of a min_y=-64 column is absolute section index -4.
        let section_y = min_y >> 4;
        world.set_blocks(-1, section_y, -1, &[(15, 0, 13, 77)]);

        assert_eq!(
            world.get(pos).unwrap().column.get_block(15, min_y, 13),
            77,
            "bulk write must route section (-1,{section_y},-1) cell (15,0,13)"
        );
        assert!(
            world.get(ChunkPos::new(0, 0)).is_none(),
            "bulk write must not touch a neighbour chunk"
        );
    }

    #[test]
    fn set_blocks_is_a_noop_for_unloaded_or_out_of_range() {
        let mut world = World::new();
        // Unloaded chunk: no panic, no creation.
        world.set_blocks(9, 0, 9, &[(0, 0, 0, 1)]);
        assert!(world.is_empty());

        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());
        let before = world.get(pos).unwrap().column.clone();
        // Section index above the column: silently ignored.
        world.set_blocks(0, 999, 0, &[(0, 0, 0, 1)]);
        assert_eq!(&world.get(pos).unwrap().column, &before);
    }

    #[test]
    fn set_block_grows_palette_single_to_indirect_to_direct_preserving_all() {
        // A fresh section is single-valued air. Setting increasingly many
        // distinct states must walk single → indirect → direct, and every
        // earlier write must still read back — a silent mis-encode renders as
        // wrong terrain, not an error, so this asserts the full re-pack.
        let mut section =
            ChunkSection::new(PaletteKind::block_states(), PaletteKind::biomes(), 0, 0);
        assert!(
            section.block_states().is_single(),
            "all-air section starts single-valued"
        );

        // One distinct non-air block → indirect (never direct for 2 values).
        section.set_block(0, 0, 0, 1);
        assert!(!section.block_states().is_single());
        assert!(
            section.block_states().palette_len() > 0,
            "two values must be an indirect palette, not direct"
        );

        // Fill distinct states 1..=257 across distinct cells. 256 distinct values
        // (+air) stays indirect at 8 bits; the 257th forces direct.
        // Cells: use the flat index space via (x,y,z) walked over the section.
        let mut expected: Vec<(usize, usize, usize, u32)> = Vec::new();
        let mut n = 0u32;
        'fill: for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    n += 1;
                    let state = n + 1; // distinct, non-air, non-colliding
                    section.set_block(x, y, z, state);
                    expected.push((x, y, z, state));
                    if n >= 300 {
                        break 'fill;
                    }
                }
            }
        }
        assert!(
            !section.block_states().is_single(),
            "many distinct values cannot be single"
        );
        assert_eq!(
            section.block_states().palette_len(),
            0,
            "past the indirect ceiling the storage must be direct (empty palette)"
        );

        // Every write survives the two re-packs intact.
        for (x, y, z, state) in expected {
            assert_eq!(
                section.get_block(x, y, z),
                state,
                "block at ({x},{y},{z}) mis-encoded across a palette upgrade"
            );
        }
    }

    #[test]
    fn merge_light_overwrites_only_named_light_sections() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());

        let mut patch = LightPatch::new();
        patch.set_sky(3, LightData::Uniform(15));
        patch.set_block(3, LightData::Uniform(7));
        world.merge_light(pos, patch);

        let light = &world.get(pos).unwrap().light;
        assert_eq!(light.sky(3), &LightData::Uniform(15), "sky light applied");
        assert_eq!(
            light.block(3),
            &LightData::Uniform(7),
            "block light applied"
        );
        assert_eq!(
            light.sky(2),
            &LightData::Missing,
            "an unnamed light section stays Missing, not defaulted"
        );
    }

    #[test]
    fn merge_light_leaves_blocks_untouched() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        let mut chunk = sample_chunk();
        chunk.column.set_block(1, 70, 1, 55);
        let column_before = chunk.column.clone();
        world.load(pos, chunk);

        let mut patch = LightPatch::new();
        patch.set_sky(4, LightData::Uniform(15));
        world.merge_light(pos, patch);

        assert_eq!(
            &world.get(pos).unwrap().column,
            &column_before,
            "a light update must never touch block state"
        );
    }

    #[test]
    fn merge_light_is_a_noop_for_an_unloaded_chunk() {
        let mut world = World::new();
        let mut patch = LightPatch::new();
        patch.set_sky(0, LightData::Uniform(15));
        // A standalone light update for a chunk we do not hold is dropped — the
        // chunk packet, when it arrives, carries its own light.
        world.merge_light(ChunkPos::new(5, 5), patch);
        assert!(
            world.is_empty(),
            "merge_light must not create an absent chunk"
        );
    }

    #[test]
    fn merge_light_ignores_out_of_range_light_sections() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());
        let light_before = world.get(pos).unwrap().light.clone();

        let mut patch = LightPatch::new();
        // A modern column has section_count + 2 light sections; this is past it.
        patch.set_sky(9999, LightData::Uniform(15));
        world.merge_light(pos, patch);

        assert_eq!(
            &world.get(pos).unwrap().light,
            &light_before,
            "an out-of-range light section must be ignored, not panic"
        );
    }

    #[test]
    fn merge_light_is_reachable_through_the_worldsink_trait() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());

        let mut patch = LightPatch::new();
        patch.set_block(2, LightData::Uniform(11));
        let sink: &mut dyn WorldSink = &mut world;
        sink.merge_light(pos, patch);

        assert_eq!(
            world.get(pos).unwrap().light.block(2),
            &LightData::Uniform(11),
            "merge_light must dispatch through &mut dyn WorldSink"
        );
    }

    /// Builds a wire-shaped light mask (`long[]`, LSB-first) from a list of set
    /// light-section indices, mirroring Minecraft's `BitSet.toLongArray()`.
    fn mask_of(bits: &[usize]) -> Vec<u64> {
        let mut m: Vec<u64> = Vec::new();
        for &b in bits {
            let word = b >> 6;
            if word >= m.len() {
                m.resize(word + 1, 0);
            }
            m[word] |= 1u64 << (b & 63);
        }
        m
    }

    #[test]
    fn from_light_masks_maps_full_arrays_and_consumes_them_in_ascending_order() {
        let sky_a = NibbleArray::from_bytes(&[0x11u8; 2048]).unwrap();
        let sky_b = NibbleArray::from_bytes(&[0x22u8; 2048]).unwrap();
        // Two set bits (sections 1 and 3); the arrays arrive in ascending-section
        // order, so array[0] must land at section 1 and array[1] at section 3.
        let patch = LightPatch::from_light_masks(
            &mask_of(&[1, 3]),
            &mask_of(&[]),
            vec![sky_a.clone(), sky_b.clone()],
            &mask_of(&[]),
            &mask_of(&[]),
            vec![],
        );

        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());
        world.merge_light(pos, patch);

        let light = &world.get(pos).unwrap().light;
        assert_eq!(
            light.sky(1),
            &LightData::Values(sky_a),
            "first array lands at the first set bit"
        );
        assert_eq!(
            light.sky(3),
            &LightData::Values(sky_b),
            "second array lands at the second set bit — arrays are consumed in \
             ascending section order, not packed into the first N sections"
        );
    }

    #[test]
    fn from_light_masks_maps_empty_mask_to_explicit_zero_not_absent() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());

        // Pre-seed section 2 to full-bright sky so we can prove the empty mask
        // actively zeroes it rather than being ignored.
        let mut seed = LightPatch::new();
        seed.set_sky(2, LightData::Uniform(15));
        world.merge_light(pos, seed);

        let patch = LightPatch::from_light_masks(
            &mask_of(&[]),
            &mask_of(&[2]),
            vec![],
            &mask_of(&[]),
            &mask_of(&[]),
            vec![],
        );
        world.merge_light(pos, patch);

        assert_eq!(
            world.get(pos).unwrap().light.sky(2),
            &LightData::Uniform(0),
            "an empty-mask section is explicitly zero light — not skipped (which \
             would leave the prior 15) and not Missing (which means absent)"
        );
    }

    #[test]
    fn from_light_masks_leaves_unnamed_sections_untouched() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());

        let mut seed = LightPatch::new();
        seed.set_sky(2, LightData::Uniform(15));
        world.merge_light(pos, seed);

        let arr = NibbleArray::from_bytes(&[0x44u8; 2048]).unwrap();
        let patch = LightPatch::from_light_masks(
            &mask_of(&[3]),
            &mask_of(&[]),
            vec![arr.clone()],
            &mask_of(&[]),
            &mask_of(&[]),
            vec![],
        );
        world.merge_light(pos, patch);

        let light = &world.get(pos).unwrap().light;
        assert_eq!(
            light.sky(2),
            &LightData::Uniform(15),
            "a section named by neither mask keeps its prior light"
        );
        assert_eq!(
            light.sky(3),
            &LightData::Values(arr),
            "the named section updates"
        );
    }

    #[test]
    fn from_light_masks_treats_sky_and_block_layers_independently() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        world.load(pos, sample_chunk());

        let sky = NibbleArray::from_bytes(&[0x33u8; 2048]).unwrap();
        // Section 2: sky carries a full array, block is explicitly empty.
        let patch = LightPatch::from_light_masks(
            &mask_of(&[2]),
            &mask_of(&[]),
            vec![sky.clone()],
            &mask_of(&[]),
            &mask_of(&[2]),
            vec![],
        );
        world.merge_light(pos, patch);

        let light = &world.get(pos).unwrap().light;
        assert_eq!(light.sky(2), &LightData::Values(sky), "sky array applied");
        assert_eq!(
            light.block(2),
            &LightData::Uniform(0),
            "block empty-mask applied to the same section independently of sky"
        );
    }

    /// Extracts the backing array of a `Values` light layer, panicking otherwise.
    fn values(data: &LightData) -> &NibbleArray {
        match data {
            LightData::Values(arr) => arr,
            other => panic!("expected Values light, got {other:?}"),
        }
    }

    #[test]
    fn section_light_snapshot_survives_relight_copy_on_write() {
        // The block-side Arc<ChunkSection> snapshot must have a light-side twin
        // that is equally immune to a later relight. Take a snapshot, mutate the
        // column's light, and prove the snapshot kept the pre-edit values and its
        // array forked (no longer shares storage) rather than being mutated
        // underneath a mesher.
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        let mut chunk = sample_chunk();
        let mut arr = NibbleArray::filled(0);
        arr.set(NibbleArray::index(1, 1, 1), 9);
        *chunk.light.sky_mut(3) = LightData::Values(arr);
        world.load(pos, chunk);

        let snap = world.section_light(pos, 3).unwrap();
        // Before the write the snapshot shares the column's array.
        let live = world.section_light(pos, 3).unwrap();
        assert!(
            values(&snap.sky).shares_storage_with(values(&live.sky)),
            "an untouched snapshot must share storage, not deep-copy"
        );

        // Relight a *different* nibble in the same section.
        world
            .get_mut(pos)
            .unwrap()
            .light
            .set_sky_light(3, NibbleArray::index(2, 2, 2), 4);

        // The snapshot is unchanged: old value present, new write absent.
        assert_eq!(values(&snap.sky).get(NibbleArray::index(1, 1, 1)), 9);
        assert_eq!(
            values(&snap.sky).get(NibbleArray::index(2, 2, 2)),
            0,
            "snapshot must not see a write made after it was taken"
        );
        // And its array forked away from the column's.
        let after = world.section_light(pos, 3).unwrap();
        assert!(
            !values(&snap.sky).shares_storage_with(values(&after.sky)),
            "the relight must fork copy-on-write, breaking the shared Arc"
        );
        assert_eq!(
            values(&after.sky).get(NibbleArray::index(2, 2, 2)),
            4,
            "the column itself must reflect the relight"
        );
    }

    #[test]
    fn section_light_is_available_for_an_elided_air_section() {
        // sample_chunk has no block sections allocated, so block section 0 is
        // elided and World::section returns None — but its sky light must still
        // be queryable, or a face meshed against that air renders black (§7).
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        let mut chunk = sample_chunk();
        // Light section 1 covers block section 0.
        *chunk.light.sky_mut(1) = LightData::Uniform(15);
        world.load(pos, chunk);

        assert!(
            world.section(pos, 0).is_none(),
            "block section 0 is elided (all air)"
        );
        assert_eq!(
            world.section_light(pos, 1),
            Some(SectionLight {
                sky: LightData::Uniform(15),
                block: LightData::Missing,
            }),
            "light must be reachable even where the block section is elided"
        );
    }

    #[test]
    fn section_light_is_none_for_unloaded_or_out_of_range() {
        let mut world = World::new();
        let pos = ChunkPos::new(0, 0);
        assert_eq!(
            world.section_light(pos, 0),
            None,
            "no light for an unloaded chunk"
        );
        world.load(pos, sample_chunk());
        // A 24-section column has 26 light sections (0..=25); 26 is out of range.
        assert_eq!(
            world.section_light(pos, 26),
            None,
            "no light for an out-of-range light section"
        );
        assert!(
            world.section_light(pos, 25).is_some(),
            "the top boundary light section is in range"
        );
    }

    #[test]
    fn light_snapshot_clone_shares_storage_until_written() {
        // The property the mesher relies on: a Values snapshot is O(1) to clone
        // (a shared Arc), and only a write forks it.
        let mut original = LightData::Values(NibbleArray::filled(3));
        let snapshot = original.clone();
        assert!(
            values(&original).shares_storage_with(values(&snapshot)),
            "clone must share the backing array"
        );
        original.set(NibbleArray::index(0, 0, 0), 12);
        assert!(
            !values(&original).shares_storage_with(values(&snapshot)),
            "a write must fork the shared array copy-on-write"
        );
        assert_eq!(values(&snapshot).get(NibbleArray::index(0, 0, 0)), 3);
        assert_eq!(values(&original).get(NibbleArray::index(0, 0, 0)), 12);
    }
}
