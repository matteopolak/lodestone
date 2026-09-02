//! Paletted, bit-packed container of opaque integer ids.
//!
//! See the crate docs for the memory rationale. A container stores `N` entries
//! (4096 for a block-state section, 64 for a biome section) using whichever of
//! three strategies is smallest for the current content, transitioning
//! automatically as values are added:
//!
//! * [`Storage::Single`] — one value, no index array.
//! * [`Storage::Indirect`] — palette + packed indices, clamped to a per-kind
//!   minimum width and used up to a per-kind ceiling.
//! * [`Storage::Direct`] — raw ids packed at a fixed width, no palette.
//!
//! The thresholds match vanilla (verified against the 26.2 sources): block
//! states clamp the indirect width up to 4 bits and switch to direct above 8;
//! biomes run 1..=3 bits indirect and switch to direct above 3.

use std::collections::HashMap;

use lodestone_core::{Reader, Writer};

use crate::packed::PackedArray;
use crate::{Result, WorldError};

/// The storage strategy and its backing data.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Storage {
    /// Every entry is this value; no index array is allocated.
    Single(u32),
    /// A palette of distinct values plus packed indices into it.
    Indirect {
        palette: Vec<u32>,
        data: PackedArray,
    },
    /// Raw ids packed directly, with no palette indirection.
    Direct(PackedArray),
}

/// Bits-per-entry width chosen for a given palette size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Config {
    Single,
    Indirect(u32),
    Direct(u32),
}

/// How the packed long array is framed on the wire.
///
/// The bit packing, palette selection, thresholds and index order are all
/// structural and version-free; only the framing of the trailing long array
/// differs between protocol families, so a version crate selects it once via
/// [`PaletteKind::with_framing`] and the codec obeys.
///
/// # Version boundary
///
/// Vanilla removed the VarInt length prefix in **1.21.5 (snapshot 25w07a,
/// protocol 770)**: from then on the long count is derived from
/// `bits_per_entry` and the entry count, and the array is written with a fixed
/// size. This was confirmed by reading the real 26.2 decoder and corroborated
/// by the Minecraft Wiki and by
/// `vendor/minecraft-data`, which shows the sibling chunk-format break at
/// exactly 1.21.5 (heightmaps switch from an NBT compound to a typed long-array
/// list). Confidence: **high**.
///
/// Protocol families **≤ 769 (≤ 1.21.4)** must select [`Prefixed`]; **≥ 770
/// (≥ 1.21.5)** use [`FixedSize`], which is this crate's default because the
/// authoritative 26.2 reference uses it.
///
/// [`Prefixed`]: LongArrayFraming::Prefixed
/// [`FixedSize`]: LongArrayFraming::FixedSize
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LongArrayFraming {
    /// The long array is prefixed with a VarInt element count (≤ 1.21.4). The
    /// decoder validates that the declared count matches the count implied by
    /// the layout, so a mismatched framing config fails cleanly.
    Prefixed,
    /// The long array has no length prefix; its length is derived from the bits
    /// and entry count (≥ 1.21.5). This is the default.
    #[default]
    FixedSize,
}

/// Describes a container's fixed geometry, its palette thresholds, and the
/// version-specific framing of its packed long array.
///
/// A [`PaletteKind`] is cheap to copy and carries no per-container state. The
/// two common kinds are [`PaletteKind::block_states`] and
/// [`PaletteKind::biomes`]; use [`PaletteKind::custom`] or the
/// `*_with_direct_bits` constructors for other versions whose global registries
/// differ in size, and [`PaletteKind::with_framing`] to select a pre-1.21.5
/// long-array framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteKind {
    entry_count: usize,
    bits_per_axis: u32,
    indirect_min_bits: u32,
    indirect_max_bits: u32,
    direct_bits: u32,
    framing: LongArrayFraming,
}

impl PaletteKind {
    /// Block-state container: 4096 entries, indirect width clamped to 4 bits,
    /// direct above 8 bits, direct width `direct_bits`.
    #[must_use]
    pub const fn block_states_with_direct_bits(direct_bits: u32) -> Self {
        Self {
            entry_count: 4096,
            bits_per_axis: 4,
            indirect_min_bits: 4,
            indirect_max_bits: 8,
            direct_bits,
            framing: LongArrayFraming::FixedSize,
        }
    }

    /// Block-state container using a 15-bit direct width, matching a modern
    /// (1.18+) global block-state registry.
    #[must_use]
    pub const fn block_states() -> Self {
        Self::block_states_with_direct_bits(15)
    }

    /// Biome container: 64 entries, indirect width 1..=3 bits, direct above 3
    /// bits, direct width `direct_bits`.
    #[must_use]
    pub const fn biomes_with_direct_bits(direct_bits: u32) -> Self {
        Self {
            entry_count: 64,
            bits_per_axis: 2,
            indirect_min_bits: 1,
            indirect_max_bits: 3,
            direct_bits,
            framing: LongArrayFraming::FixedSize,
        }
    }

    /// Biome container using a 6-bit direct width.
    #[must_use]
    pub const fn biomes() -> Self {
        Self::biomes_with_direct_bits(6)
    }

    /// Fully custom kind. `entry_count` becomes `(1 << bits_per_axis)^3`.
    ///
    /// # Panics
    /// Panics if the widths are inconsistent (`min < 1`, `min > max`,
    /// `max >= direct`, or `direct` exceeds [`PackedArray::MAX_BITS`]).
    #[must_use]
    pub const fn custom(
        bits_per_axis: u32,
        indirect_min_bits: u32,
        indirect_max_bits: u32,
        direct_bits: u32,
    ) -> Self {
        assert!(indirect_min_bits >= 1, "indirect_min_bits must be >= 1");
        assert!(
            indirect_min_bits <= indirect_max_bits,
            "indirect_min_bits must not exceed indirect_max_bits"
        );
        assert!(
            indirect_max_bits < direct_bits,
            "direct_bits must exceed indirect_max_bits"
        );
        assert!(
            direct_bits <= PackedArray::MAX_BITS,
            "direct_bits too large"
        );
        Self {
            entry_count: 1 << (bits_per_axis * 3),
            bits_per_axis,
            indirect_min_bits,
            indirect_max_bits,
            direct_bits,
            framing: LongArrayFraming::FixedSize,
        }
    }

    /// Returns this kind with the long-array wire framing overridden. Version
    /// crates for **≤ 1.21.4 (protocol ≤ 769)** should call
    /// `.with_framing(LongArrayFraming::Prefixed)`; newer families keep the
    /// [`LongArrayFraming::FixedSize`] default.
    #[must_use]
    pub const fn with_framing(mut self, framing: LongArrayFraming) -> Self {
        self.framing = framing;
        self
    }

    /// The long-array wire framing this kind uses.
    #[must_use]
    pub const fn framing(&self) -> LongArrayFraming {
        self.framing
    }

    /// Number of entries in a container of this kind.
    #[must_use]
    pub const fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Length of one edge of the cube (`1 << bits_per_axis`).
    #[must_use]
    pub const fn edge(&self) -> usize {
        1 << self.bits_per_axis
    }

    /// Flat entry index for local coordinates, using vanilla's YZX order
    /// (`index = (y << b | z) << b | x`).
    ///
    /// # Panics
    /// Panics if any coordinate is outside `0..edge()`.
    #[must_use]
    pub fn index(&self, x: usize, y: usize, z: usize) -> usize {
        let edge = self.edge();
        assert!(x < edge && y < edge && z < edge, "coordinate out of range");
        ((y << self.bits_per_axis | z) << self.bits_per_axis) | x
    }

    fn config_for_palette_size(&self, size: usize) -> Config {
        let needed = bits_for_size(size);
        if needed == 0 {
            Config::Single
        } else if needed <= self.indirect_max_bits {
            Config::Indirect(needed.max(self.indirect_min_bits))
        } else {
            Config::Direct(self.direct_bits)
        }
    }
}

/// Bits required to distinguish `size` values: `ceil(log2(size))`.
fn bits_for_size(size: usize) -> u32 {
    if size <= 1 {
        0
    } else {
        usize::BITS - (size - 1).leading_zeros()
    }
}

/// A paletted, bit-packed container of opaque non-negative integer ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PalettedContainer {
    kind: PaletteKind,
    storage: Storage,
}

impl PalettedContainer {
    /// Creates a container of `kind` with every entry set to `value`.
    #[must_use]
    pub fn new(kind: PaletteKind, value: u32) -> Self {
        Self {
            kind,
            storage: Storage::Single(value),
        }
    }

    /// Builds a container from a full slice of `entry_count` values, choosing
    /// the smallest storage strategy that fits.
    ///
    /// This is the efficient path for a version crate that has already decoded
    /// or computed a whole section's worth of ids.
    ///
    /// # Panics
    /// Panics if `values.len() != kind.entry_count()`.
    #[must_use]
    pub fn from_values(kind: PaletteKind, values: &[u32]) -> Self {
        assert_eq!(
            values.len(),
            kind.entry_count(),
            "expected {} values",
            kind.entry_count()
        );
        Self {
            kind,
            storage: build_storage(kind, values),
        }
    }

    /// Builds a cube container from a **2-D `(x, z)` source that is constant over
    /// `y`** — the shape legacy chunk data (≤ 1.12) carries for biomes: one biome
    /// per column, with no vertical variation.
    ///
    /// This is the single sanctioned place to bridge that legacy 2-D biome map
    /// into the version-free 3-D biome container, so a version crate does not
    /// hand-roll the mapping. `source_edge` is the width of the square source
    /// grid (16 for legacy's 16×16 per-block array); `at(sx, sz)` returns the
    /// source value at source cell `(sx, sz)`, each in `0..source_edge`. Biome
    /// cell `b` samples source cell `b * source_edge / edge` (nearest-cell
    /// down-sampling), and the sampled value is written to **every** `y` layer.
    ///
    /// Two honesty notes on fidelity, because this is a lossy bridge in one
    /// direction and exact in the other:
    /// - Replicating across `y` is **exact**, not invented structure: the source
    ///   genuinely has no vertical dimension, so every layer must be equal.
    /// - The horizontal `source_edge → edge` reduction (16 → 4 for biomes) is
    ///   **lossy** and unavoidable at the container's biome resolution — it keeps
    ///   1/16 of the horizontal cells the legacy server sent. A representation
    ///   that preserves the full 16×16 needs a column-level 2-D biome store,
    ///   which is a deliberately separate, larger change (it alters how every
    ///   consumer queries biomes); this constructor is the additive step that at
    ///   least centralises and documents the reduction.
    ///
    /// # Panics
    /// Panics if `source_edge` is `0`.
    #[must_use]
    pub fn from_2d_source(
        kind: PaletteKind,
        source_edge: usize,
        at: impl Fn(usize, usize) -> u32,
    ) -> Self {
        assert!(source_edge > 0, "source_edge must be positive");
        let edge = kind.edge();
        let mut values = vec![0u32; kind.entry_count()];
        for y in 0..edge {
            for z in 0..edge {
                for x in 0..edge {
                    let sx = x * source_edge / edge;
                    let sz = z * source_edge / edge;
                    values[kind.index(x, y, z)] = at(sx, sz);
                }
            }
        }
        Self::from_values(kind, &values)
    }

    /// The container's kind.
    #[must_use]
    pub const fn kind(&self) -> PaletteKind {
        self.kind
    }

    /// Number of entries (fixed by the kind).
    #[must_use]
    pub const fn entry_count(&self) -> usize {
        self.kind.entry_count
    }

    /// Bits-per-entry currently in use (`0` for a single-value container).
    #[must_use]
    pub fn bits_per_entry(&self) -> u32 {
        match &self.storage {
            Storage::Single(_) => 0,
            Storage::Indirect { data, .. } | Storage::Direct(data) => data.bits(),
        }
    }

    /// Number of palette entries. Single-value reports 1; direct reports 0
    /// because it has no palette (ids are global).
    #[must_use]
    pub fn palette_len(&self) -> usize {
        match &self.storage {
            Storage::Single(_) => 1,
            Storage::Indirect { palette, .. } => palette.len(),
            Storage::Direct(_) => 0,
        }
    }

    /// Returns the single value if the whole container holds exactly one.
    #[must_use]
    pub fn single_value(&self) -> Option<u32> {
        match &self.storage {
            Storage::Single(v) => Some(*v),
            _ => None,
        }
    }

    /// Returns `true` when no index array is allocated (single-value storage).
    #[must_use]
    pub fn is_single(&self) -> bool {
        matches!(self.storage, Storage::Single(_))
    }

    /// Returns the value at flat `index`.
    ///
    /// # Panics
    /// Panics if `index >= entry_count`.
    #[must_use]
    pub fn get(&self, index: usize) -> u32 {
        assert!(index < self.kind.entry_count, "index out of range");
        match &self.storage {
            Storage::Single(v) => *v,
            Storage::Indirect { palette, data } => palette[data.get(index) as usize],
            Storage::Direct(data) => data.get(index),
        }
    }

    /// Sets flat `index` to `value`, transitioning storage as needed.
    ///
    /// # Panics
    /// Panics if `index >= entry_count`.
    pub fn set(&mut self, index: usize, value: u32) {
        assert!(index < self.kind.entry_count, "index out of range");
        match &mut self.storage {
            Storage::Direct(data) => {
                data.set(index, value);
                return;
            }
            Storage::Indirect { palette, data } => {
                if let Some(id) = palette.iter().position(|&p| p == value) {
                    data.set(index, id as u32);
                    return;
                }
                if bits_for_size(palette.len() + 1) <= data.bits() {
                    let id = palette.len() as u32;
                    palette.push(value);
                    data.set(index, id);
                    return;
                }
                // Palette outgrew the current width: fall through to rebuild.
            }
            Storage::Single(v) => {
                if *v == value {
                    return;
                }
                // Second distinct value: fall through to rebuild.
            }
        }
        self.rebuild_with(index, value);
    }

    /// Slow path: snapshot every value, apply the pending write, and rebuild the
    /// storage at the strategy the new palette size demands. Only reached when a
    /// write forces a widening or a strategy transition.
    fn rebuild_with(&mut self, index: usize, value: u32) {
        let n = self.kind.entry_count;
        let mut values: Vec<u32> = (0..n).map(|i| self.get(i)).collect();
        values[index] = value;
        self.storage = build_storage(self.kind, &values);
    }

    /// Iterates every entry in flat index order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.kind.entry_count).map(move |i| self.get(i))
    }

    /// Heap bytes owned by this container (palette and packed longs).
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.palette_heap_bytes() + self.packed_heap_bytes()
    }

    /// Heap bytes owned by the palette `Vec<u32>` alone (`0` for [`Single`] and
    /// [`Direct`], which hold no palette).
    ///
    /// Split out from [`heap_bytes`](Self::heap_bytes) for the
    /// pool-footprint question: the palette is the allocation with **no length
    /// guard**, growing by ordinary `Vec` push-doubling and never shrinking on
    /// its own, unlike the packed array's fixed size classes (see
    /// [`packed_heap_bytes`](Self::packed_heap_bytes)). A caller sizing a
    /// recycling strategy needs to see these two numbers separately, not just
    /// their sum.
    ///
    /// [`Single`]: Storage::Single
    /// [`Direct`]: Storage::Direct
    #[must_use]
    pub fn palette_heap_bytes(&self) -> usize {
        match &self.storage {
            Storage::Indirect { palette, .. } => palette.capacity() * core::mem::size_of::<u32>(),
            Storage::Single(_) | Storage::Direct(_) => 0,
        }
    }

    /// Heap bytes owned by the packed index array alone (`0` for [`Single`],
    /// which allocates none). This is the size-classed allocation a pool
    /// would recycle; see [`palette_heap_bytes`](Self::palette_heap_bytes)
    /// for the other half of [`heap_bytes`](Self::heap_bytes).
    ///
    /// [`Single`]: Storage::Single
    #[must_use]
    pub fn packed_heap_bytes(&self) -> usize {
        match &self.storage {
            Storage::Single(_) => 0,
            Storage::Indirect { data, .. } | Storage::Direct(data) => data.heap_bytes(),
        }
    }

    /// Writes the container in Minecraft's paletted-container wire format: a
    /// bits-per-entry byte, the palette (absent for direct, a single varint for
    /// single-value, a varint count plus entries for indirect), then the packed
    /// longs. The long array is framed per this kind's
    /// [`LongArrayFraming`]: a fixed-size array with no length prefix (1.21.5+,
    /// the default) or preceded by a VarInt element count (≤ 1.21.4).
    pub fn encode(&self, w: &mut Writer) {
        match &self.storage {
            Storage::Single(v) => {
                w.u8(0);
                w.var_i32(*v as i32);
            }
            Storage::Indirect { palette, data } => {
                w.u8(data.bits() as u8);
                w.var_i32(palette.len() as i32);
                for &entry in palette {
                    w.var_i32(entry as i32);
                }
                write_longs(self.kind.framing, data, w);
            }
            Storage::Direct(data) => {
                w.u8(data.bits() as u8);
                write_longs(self.kind.framing, data, w);
            }
        }
    }

    /// Reads a container of `kind` from the paletted-container wire format.
    ///
    /// All lengths are bounded by the kind, and every packed index is validated
    /// against the palette, so malformed or hostile input yields a
    /// [`WorldError`] rather than a panic or an unbounded allocation.
    ///
    /// # Errors
    /// Returns [`WorldError`] for an out-of-range bits-per-entry, a bad palette
    /// length, an index that escapes the palette, or truncated input.
    pub fn decode(kind: PaletteKind, r: &mut Reader<'_>) -> Result<Self> {
        let bits = u32::from(r.u8()?);
        let n = kind.entry_count;

        if bits == 0 {
            let value = r.var_i32()? as u32;
            return Ok(Self {
                kind,
                storage: Storage::Single(value),
            });
        }
        if bits > PackedArray::MAX_BITS {
            return Err(WorldError::InvalidBits {
                bits,
                entry_count: n,
            });
        }

        if bits <= kind.indirect_max_bits {
            let count = r.var_i32()?;
            if count < 1 || count as usize > n {
                return Err(WorldError::InvalidPaletteLength(i64::from(count)));
            }
            let mut palette = Vec::with_capacity(count as usize);
            for _ in 0..count {
                palette.push(r.var_i32()? as u32);
            }
            let data = read_longs(kind.framing, bits, n, r)?;
            for i in 0..n {
                let id = data.get(i);
                if id as usize >= palette.len() {
                    return Err(WorldError::PaletteIndexOutOfRange {
                        index: id,
                        palette_len: palette.len(),
                    });
                }
            }
            Ok(Self {
                kind,
                storage: Storage::Indirect { palette, data },
            })
        } else {
            let data = read_longs(kind.framing, bits, n, r)?;
            Ok(Self {
                kind,
                storage: Storage::Direct(data),
            })
        }
    }
}

/// Writes the packed longs, honouring the version-specific framing. `Prefixed`
/// families precede the array with a VarInt element count; `FixedSize` families
/// derive the count from the layout and write the longs bare.
fn write_longs(framing: LongArrayFraming, data: &PackedArray, w: &mut Writer) {
    let longs = data.longs();
    if framing == LongArrayFraming::Prefixed {
        w.var_i32(longs.len() as i32);
    }
    for &long in longs {
        w.i64(long as i64);
    }
}

/// Reads exactly `long_count(bits, n)` big-endian longs into a [`PackedArray`].
///
/// For [`LongArrayFraming::Prefixed`] the VarInt element count is read first and
/// validated against the count implied by the layout, so decoding fixed-size
/// bytes with a prefixed config (or vice versa) fails cleanly with a
/// [`WorldError`] rather than silently mis-parsing into garbage.
fn read_longs(
    framing: LongArrayFraming,
    bits: u32,
    n: usize,
    r: &mut Reader<'_>,
) -> Result<PackedArray> {
    let long_count = PackedArray::long_count(bits, n);
    if framing == LongArrayFraming::Prefixed {
        let declared = r.var_i32()?;
        if declared < 0 || declared as usize != long_count {
            return Err(WorldError::WrongLongCount {
                expected: long_count,
                actual: declared.max(0) as usize,
            });
        }
    }
    let mut longs = Vec::with_capacity(long_count);
    for _ in 0..long_count {
        longs.push(r.i64()? as u64);
    }
    PackedArray::from_longs(bits, n, longs)
}

/// Chooses and builds the smallest storage strategy for a full value slice.
fn build_storage(kind: PaletteKind, values: &[u32]) -> Storage {
    let first = values[0];
    if values.iter().all(|&v| v == first) {
        return Storage::Single(first);
    }

    let mut id_of: HashMap<u32, u32> = HashMap::new();
    let mut palette: Vec<u32> = Vec::new();
    for &v in values {
        id_of.entry(v).or_insert_with(|| {
            let id = palette.len() as u32;
            palette.push(v);
            id
        });
    }

    match kind.config_for_palette_size(palette.len()) {
        Config::Single => Storage::Single(first),
        Config::Indirect(bits) => {
            let ids: Vec<u32> = values.iter().map(|v| id_of[v]).collect();
            Storage::Indirect {
                palette,
                data: PackedArray::from_values(bits, &ids),
            }
        }
        Config::Direct(bits) => Storage::Direct(PackedArray::from_values(bits, values)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_bytes(kind: PaletteKind, bytes: &[u8]) -> Result<PalettedContainer> {
        let mut r = Reader::new(bytes);
        PalettedContainer::decode(kind, &mut r)
    }

    #[test]
    fn from_2d_source_replicates_over_y_without_inventing_structure() {
        // A 4×4 source maps 1:1 into the biome grid and must be identical on
        // every Y layer — the legacy data has no vertical dimension, so equal
        // layers are exact, and any y-variation would be fabricated.
        let kind = PaletteKind::biomes();
        let source = |sx: usize, sz: usize| (sx + sz * 4) as u32;
        let c = PalettedContainer::from_2d_source(kind, 4, source);
        for y in 0..4 {
            for z in 0..4 {
                for x in 0..4 {
                    assert_eq!(
                        c.get(kind.index(x, y, z)),
                        source(x, z),
                        "biome ({x},{y},{z}) must equal its column source"
                    );
                }
            }
        }
    }

    #[test]
    fn from_2d_source_downsamples_16_to_4_by_nearest_cell() {
        // The 16×16 legacy array reduces to 4×4: biome cell b samples source
        // cell b*4 (0, 4, 8, 12). Bias the source so a nearest-cell sample (b*4)
        // is distinguishable from a centre sample (b*4+2), and keep every sampled
        // value inside the biome container's 6-bit width.
        let kind = PaletteKind::biomes();
        let source = |sx: usize, sz: usize| (sx * 4 + sz / 4) as u32;
        let c = PalettedContainer::from_2d_source(kind, 16, source);
        for bz in 0..4 {
            for bx in 0..4 {
                assert_eq!(
                    c.get(kind.index(bx, 0, bz)),
                    (bx * 16 + bz) as u32,
                    "biome cell ({bx},{bz}) must sample source cell ({},{}), not its centre",
                    bx * 4,
                    bz * 4
                );
            }
        }
    }

    #[test]
    fn from_2d_source_uniform_source_collapses_to_single() {
        // A 1×1 (uniform) source, or any constant source, must land as a
        // single-value container — no palette, no packed array.
        let kind = PaletteKind::biomes();
        let c = PalettedContainer::from_2d_source(kind, 1, |_, _| 7);
        assert_eq!(c.single_value(), Some(7));
        let c2 = PalettedContainer::from_2d_source(kind, 16, |_, _| 3);
        assert_eq!(c2.single_value(), Some(3));
    }

    #[test]
    fn bits_for_size_matches_ceil_log2() {
        for (n, bits) in [
            (0, 0),
            (1, 0),
            (2, 1),
            (3, 2),
            (4, 2),
            (5, 3),
            (16, 4),
            (17, 5),
            (256, 8),
            (257, 9),
        ] {
            assert_eq!(bits_for_size(n), bits, "size {n}");
        }
    }

    #[test]
    fn single_value_stores_no_index_array() {
        let c = PalettedContainer::new(PaletteKind::block_states(), 42);
        for i in 0..c.entry_count() {
            assert_eq!(c.get(i), 42);
        }
        assert_eq!(c.bits_per_entry(), 0);
        assert_eq!(c.palette_len(), 1);
        assert_eq!(c.single_value(), Some(42));
        assert!(c.is_single());
        assert_eq!(c.heap_bytes(), 0, "single value must not allocate");

        // Wire form is exactly [0][varint value] with no longs.
        let mut w = Writer::default();
        c.encode(&mut w);
        assert_eq!(w.as_slice(), &[0x00, 42]);
    }

    #[test]
    fn single_to_indirect_preserves_all_entries() {
        let kind = PaletteKind::block_states();
        let mut c = PalettedContainer::new(kind, 7);
        c.set(100, 9);
        // Now indirect: palette {7, 9}, clamped to the 4-bit block minimum.
        assert_eq!(c.bits_per_entry(), 4);
        assert_eq!(c.palette_len(), 2);
        assert!(!c.is_single());
        for i in 0..c.entry_count() {
            let expected = if i == 100 { 9 } else { 7 };
            assert_eq!(c.get(i), expected, "entry {i}");
        }
    }

    #[test]
    fn indirect_widens_then_transitions_to_direct() {
        let kind = PaletteKind::block_states();
        // Fill entries 0..300 with distinct values; the rest stay 0.
        let mut c = PalettedContainer::new(kind, 0);
        for i in 0..300 {
            c.set(i, (1000 + i) as u32);
        }
        // 300 distinct values plus the background 0 => 301 distinct => 9 bits
        // needed, which is above the block ceiling of 8, so storage is direct.
        assert_eq!(c.palette_len(), 0, "direct has no palette");
        assert_eq!(c.bits_per_entry(), 15, "direct width for block states");
        for i in 0..c.entry_count() {
            let expected = if i < 300 { (1000 + i) as u32 } else { 0 };
            assert_eq!(c.get(i), expected, "entry {i}");
        }
    }

    #[test]
    fn indirect_widens_within_ceiling() {
        let kind = PaletteKind::block_states();
        let mut c = PalettedContainer::new(kind, 0);
        // 20 distinct values plus background 0 => 21 distinct => 5 bits
        // (> 4-bit floor, <= 8-bit ceiling).
        for i in 0..20 {
            c.set(i, (500 + i) as u32);
        }
        assert_eq!(c.palette_len(), 21);
        assert_eq!(c.bits_per_entry(), 5);
        for i in 0..c.entry_count() {
            let expected = if i < 20 { (500 + i) as u32 } else { 0 };
            assert_eq!(c.get(i), expected);
        }
    }

    #[test]
    fn wire_round_trip_single() {
        let kind = PaletteKind::block_states();
        let c = PalettedContainer::new(kind, 1234);
        assert_round_trip(kind, &c);
    }

    #[test]
    fn wire_round_trip_indirect() {
        let kind = PaletteKind::block_states();
        let mut c = PalettedContainer::new(kind, 0);
        for i in 0..30 {
            c.set(i * 7, (i + 1) as u32);
        }
        assert!(matches!(c.storage, Storage::Indirect { .. }));
        assert_round_trip(kind, &c);
    }

    #[test]
    fn wire_round_trip_direct() {
        let kind = PaletteKind::block_states();
        let mut c = PalettedContainer::new(kind, 0);
        for i in 0..400 {
            c.set(i, (1 + i) as u32);
        }
        assert!(matches!(c.storage, Storage::Direct(_)));
        assert_round_trip(kind, &c);
    }

    #[test]
    fn wire_round_trip_biomes() {
        let kind = PaletteKind::biomes();
        let mut c = PalettedContainer::new(kind, 3);
        c.set(0, 5);
        c.set(1, 8);
        assert_round_trip(kind, &c);
    }

    fn prefixed_indirect() -> (PaletteKind, PalettedContainer) {
        let kind = PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed);
        let mut c = PalettedContainer::new(kind, 0);
        for i in 0..30 {
            c.set(i * 7, (i + 1) as u32);
        }
        assert!(matches!(c.storage, Storage::Indirect { .. }));
        (kind, c)
    }

    #[test]
    fn prefixed_framing_round_trips_all_strategies() {
        let kind = PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed);
        assert_eq!(kind.framing(), LongArrayFraming::Prefixed);

        // Single stores no long array, so no prefix is emitted either way.
        let single = PalettedContainer::new(kind, 1234);
        assert_round_trip(kind, &single);

        let (_, indirect) = prefixed_indirect();
        assert_round_trip(kind, &indirect);

        let mut direct = PalettedContainer::new(kind, 0);
        for i in 0..400 {
            direct.set(i, (1 + i) as u32);
        }
        assert!(matches!(direct.storage, Storage::Direct(_)));
        assert_round_trip(kind, &direct);
    }

    #[test]
    fn prefixed_indirect_emits_varint_long_count() {
        let (_, c) = prefixed_indirect();
        let mut w = Writer::default();
        c.encode(&mut w);
        let bytes = w.into_vec();

        // Byte 0 is bits-per-entry; skip the palette, then the VarInt long count
        // must equal the fixed-size layout count.
        let Storage::Indirect { palette, data } = &c.storage else {
            unreachable!();
        };
        let mut r = Reader::new(&bytes);
        assert_eq!(u32::from(r.u8().unwrap()), data.bits());
        assert_eq!(r.var_i32().unwrap() as usize, palette.len());
        for _ in 0..palette.len() {
            r.var_i32().unwrap();
        }
        assert_eq!(r.var_i32().unwrap() as usize, data.longs().len());
    }

    #[test]
    fn fixed_size_bytes_decoded_as_prefixed_fail_cleanly() {
        // Encode with the modern fixed-size framing, decode expecting a prefix.
        let fixed = PaletteKind::block_states();
        let mut c = PalettedContainer::new(fixed, 0);
        for i in 0..30 {
            c.set(i * 7, (i + 1) as u32);
        }
        let mut w = Writer::default();
        c.encode(&mut w);
        let bytes = w.into_vec();

        let prefixed = PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed);
        let err = decode_bytes(prefixed, &bytes).unwrap_err();
        // The first long is read as the "declared count"; it will not match the
        // real layout, so we reject rather than mis-parse.
        assert!(
            matches!(err, WorldError::WrongLongCount { .. } | WorldError::Core(_)),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn prefixed_bytes_decoded_as_fixed_size_fail_cleanly() {
        let (_, c) = prefixed_indirect();
        let mut w = Writer::default();
        c.encode(&mut w);
        let bytes = w.into_vec();

        // Decoding prefixed bytes with a fixed-size config consumes the VarInt
        // length prefix as packed-long data. This is caught one of two ways:
        // either the misaligned indices escape the palette (an explicit error),
        // or the stray prefix bytes are left unconsumed at the end. The chunk
        // decoder asserts the section blob is fully drained, so trailing bytes
        // are a clean, detectable failure rather than a silent misparse.
        let fixed = PaletteKind::block_states();
        let mut r = Reader::new(&bytes);
        match PalettedContainer::decode(fixed, &mut r) {
            Err(WorldError::PaletteIndexOutOfRange { .. } | WorldError::Core(_)) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
            Ok(_) => assert!(
                !r.is_empty(),
                "fixed-size decode of prefixed bytes must not fully consume the buffer"
            ),
        }
    }

    #[test]
    fn wrong_prefixed_long_count_is_rejected() {
        // Hand-build an indirect prefixed container whose declared long count is
        // one too large: it must be rejected before allocating.
        let kind = PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed);
        let real = PackedArray::long_count(4, 4096);
        let mut w = Writer::default();
        w.u8(4);
        w.var_i32(2); // palette length
        w.var_i32(0);
        w.var_i32(1);
        w.var_i32((real + 1) as i32); // lies about the long count
        for _ in 0..(real + 1) {
            w.i64(0);
        }
        let err = decode_bytes(kind, &w.into_vec()).unwrap_err();
        assert!(matches!(
            err,
            WorldError::WrongLongCount {
                expected,
                actual,
            } if expected == real && actual == real + 1
        ));
    }

    fn assert_round_trip(kind: PaletteKind, c: &PalettedContainer) {
        let mut w = Writer::default();
        c.encode(&mut w);
        let bytes = w.into_vec();

        let decoded = decode_bytes(kind, &bytes).expect("decode");
        for i in 0..kind.entry_count() {
            assert_eq!(decoded.get(i), c.get(i), "entry {i}");
        }

        // Re-encoding the decoded container reproduces the exact bytes.
        let mut w2 = Writer::default();
        decoded.encode(&mut w2);
        assert_eq!(w2.as_slice(), bytes.as_slice(), "re-encode is not stable");
    }

    #[test]
    fn decode_rejects_out_of_range_bits() {
        let err = decode_bytes(PaletteKind::block_states(), &[40]).unwrap_err();
        assert!(matches!(err, WorldError::InvalidBits { bits: 40, .. }));
    }

    #[test]
    fn decode_rejects_bad_palette_length() {
        // bits = 4 (indirect), palette count = 0 is illegal.
        let err = decode_bytes(PaletteKind::block_states(), &[4, 0]).unwrap_err();
        assert!(matches!(err, WorldError::InvalidPaletteLength(0)));
    }

    #[test]
    fn decode_rejects_truncated_longs() {
        // bits = 4, palette [1, 2], but no long data follows.
        let err = decode_bytes(PaletteKind::block_states(), &[4, 2, 1, 2]).unwrap_err();
        assert!(matches!(err, WorldError::Core(_)));
    }

    #[test]
    fn decode_rejects_index_out_of_palette() {
        // bits = 4, palette of length 2, but the packed data references id 3.
        let mut bytes = vec![4u8, 2, 10, 20];
        let long_count = PackedArray::long_count(4, 4096);
        for i in 0..long_count {
            let long: u64 = if i == 0 { 3 } else { 0 };
            bytes.extend_from_slice(&long.to_be_bytes());
        }
        let err = decode_bytes(PaletteKind::block_states(), &bytes).unwrap_err();
        assert!(matches!(
            err,
            WorldError::PaletteIndexOutOfRange {
                index: 3,
                palette_len: 2
            }
        ));
    }

    #[test]
    fn from_values_picks_single_for_uniform_input() {
        let kind = PaletteKind::block_states();
        let values = vec![5u32; kind.entry_count()];
        let c = PalettedContainer::from_values(kind, &values);
        assert!(c.is_single());
        assert_eq!(c.single_value(), Some(5));
    }

    #[test]
    fn palette_and_packed_heap_bytes_split_sum_to_heap_bytes() {
        let kind = PaletteKind::block_states();

        // Single: both halves are zero.
        let single = PalettedContainer::new(kind, 7);
        assert_eq!(single.palette_heap_bytes(), 0);
        assert_eq!(single.packed_heap_bytes(), 0);
        assert_eq!(single.heap_bytes(), 0);

        // Indirect: palette half is non-zero and accounts for the gap between
        // heap_bytes() and the packed array alone.
        let mut indirect = PalettedContainer::new(kind, 0);
        for i in 0..30 {
            indirect.set(i * 7, (i + 1) as u32);
        }
        assert!(indirect.palette_heap_bytes() > 0, "indirect must own a palette");
        assert!(indirect.packed_heap_bytes() > 0, "indirect must own packed longs too");
        assert_eq!(
            indirect.palette_heap_bytes() + indirect.packed_heap_bytes(),
            indirect.heap_bytes(),
            "the split must sum to the combined total"
        );

        // Direct: palette half is zero (no palette at all), packed half carries
        // everything.
        let mut direct = PalettedContainer::new(kind, 0);
        for i in 0..400 {
            direct.set(i, (1 + i) as u32);
        }
        assert!(matches!(direct.storage, Storage::Direct(_)));
        assert_eq!(direct.palette_heap_bytes(), 0);
        assert_eq!(direct.packed_heap_bytes(), direct.heap_bytes());
    }

    #[test]
    fn from_values_picks_direct_for_high_variety() {
        let kind = PaletteKind::block_states();
        let values: Vec<u32> = (0..kind.entry_count()).map(|i| (i % 1000) as u32).collect();
        let c = PalettedContainer::from_values(kind, &values);
        assert_eq!(c.palette_len(), 0);
        assert_eq!(c.bits_per_entry(), 15);
        for i in 0..kind.entry_count() {
            assert_eq!(c.get(i), (i % 1000) as u32);
        }
    }
}
