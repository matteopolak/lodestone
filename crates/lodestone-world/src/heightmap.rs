//! Column heightmaps: 256 packed heights, one per XZ column.
//!
//! A heightmap stores, for each of the 16×16 columns in a chunk, a height in
//! `0..=world_height` using the same non-straddling bit packing as block
//! containers (vanilla builds them on a `SimpleBitStorage`, so the layout and
//! straddling rule are identical to [`PackedArray`]). The bit width is derived
//! from the world height: `ceil(log2(world_height + 1))`.
//!
//! # Version-specific framing
//!
//! Only the *framing* of the heightmap collection is version-conditional, and
//! it changed at the same 1.21.5 boundary as the paletted-container long array:
//!
//! * **≤ 1.21.4** — the chunk packet carries an NBT compound whose keys are the
//!   heightmap type names (`MOTION_BLOCKING`, …) and whose values are
//!   `LongArray` tags. A version crate reads that with `lodestone-core`'s NBT
//!   reader and feeds each long array into [`Heightmap::from_longs`].
//! * **≥ 1.21.5** — a plain typed list: a VarInt count, then per entry a VarInt
//!   registry id and a VarInt-prefixed long array. That form is
//!   [`Heightmaps::decode`]/[`Heightmaps::encode`] here.
//!
//! The packed storage itself is shared and version-free either way; only the
//! outer framing differs, so it is the one thing a version crate supplies.

use lodestone_core::{Reader, Writer};

use crate::packed::PackedArray;
use crate::{Result, WorldError};

/// Number of columns in a chunk (16 × 16).
const COLUMNS: usize = 256;

/// Bits needed to store a height in `0..=world_height`.
#[must_use]
pub fn height_bits(world_height: u32) -> u32 {
    let values = world_height + 1;
    if values <= 1 {
        0
    } else {
        u32::BITS - (values - 1).leading_zeros()
    }
}

/// A single heightmap: 256 packed heights in XZ order (`index = x + z * 16`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heightmap {
    data: PackedArray,
}

impl Heightmap {
    /// Creates an all-zero heightmap sized for a world of `world_height` blocks.
    #[must_use]
    pub fn new(world_height: u32) -> Self {
        Self {
            data: PackedArray::new(height_bits(world_height), COLUMNS),
        }
    }

    /// Rebuilds a heightmap from a packed long array (as received on the wire).
    ///
    /// # Errors
    /// Returns [`WorldError`] if the long count does not match the layout for
    /// this world height.
    pub fn from_longs(world_height: u32, longs: Vec<u64>) -> Result<Self> {
        Ok(Self {
            data: PackedArray::from_longs(height_bits(world_height), COLUMNS, longs)?,
        })
    }

    /// Flat column index for local coordinates (`x + z * 16`).
    ///
    /// # Panics
    /// Panics if `x` or `z` is outside `0..16`.
    #[must_use]
    pub fn index(x: usize, z: usize) -> usize {
        assert!(x < 16 && z < 16, "coordinate out of range");
        x + z * 16
    }

    /// Reads the stored height for column `(x, z)`.
    #[must_use]
    pub fn get(&self, x: usize, z: usize) -> u32 {
        self.data.get(Self::index(x, z))
    }

    /// Writes the height for column `(x, z)`.
    pub fn set(&mut self, x: usize, z: usize, height: u32) {
        self.data.set(Self::index(x, z), height);
    }

    /// The packed backing longs, for re-encoding or pooling.
    #[must_use]
    pub fn longs(&self) -> &[u64] {
        self.data.longs()
    }

    /// Heap bytes owned by this heightmap's packed store.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.data.heap_bytes()
    }
}

/// A chunk's heightmaps, keyed by version-specific registry type id.
///
/// The crate stays version-free by keying on the numeric registry id used by
/// the 1.21.5+ typed-list wire form; a version crate maps between those ids (or
/// the older NBT string keys) and its own semantics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heightmaps {
    maps: Vec<(u32, Heightmap)>,
}

impl Heightmaps {
    /// Creates an empty heightmap set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of heightmaps present.
    #[must_use]
    pub fn len(&self) -> usize {
        self.maps.len()
    }

    /// Whether no heightmaps are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.maps.is_empty()
    }

    /// Inserts or replaces the heightmap for `type_id`.
    pub fn insert(&mut self, type_id: u32, map: Heightmap) {
        if let Some(slot) = self.maps.iter_mut().find(|(id, _)| *id == type_id) {
            slot.1 = map;
        } else {
            self.maps.push((type_id, map));
        }
    }

    /// Returns the heightmap for `type_id`, if present.
    #[must_use]
    pub fn get(&self, type_id: u32) -> Option<&Heightmap> {
        self.maps
            .iter()
            .find(|(id, _)| *id == type_id)
            .map(|(_, m)| m)
    }

    /// Iterates `(type_id, heightmap)` pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &Heightmap)> + '_ {
        self.maps.iter().map(|(id, m)| (*id, m))
    }

    /// Heap bytes owned by all heightmaps plus the index vector.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        let maps: usize = self.maps.iter().map(|(_, m)| m.heap_bytes()).sum();
        maps + self.maps.capacity() * size_of::<(u32, Heightmap)>()
    }

    /// Reads the 1.21.5+ typed-list heightmap framing: a VarInt count, then per
    /// entry a VarInt registry id and a VarInt-prefixed long array.
    ///
    /// # Errors
    /// Returns [`WorldError`] on a negative count/length or a long array whose
    /// length disagrees with the layout for `world_height`.
    pub fn decode(world_height: u32, r: &mut Reader<'_>) -> Result<Self> {
        let count = r.var_i32()?;
        if count < 0 {
            return Err(WorldError::InvalidPaletteLength(i64::from(count)));
        }
        let expected = PackedArray::long_count(height_bits(world_height), COLUMNS);
        let mut maps = Vec::with_capacity((count as usize).min(64));
        for _ in 0..count {
            let type_id = r.var_i32()? as u32;
            let long_count = r.var_i32()?;
            if long_count < 0 || long_count as usize != expected {
                return Err(WorldError::WrongLongCount {
                    expected,
                    actual: long_count.max(0) as usize,
                });
            }
            let mut longs = Vec::with_capacity(expected);
            for _ in 0..expected {
                longs.push(r.i64()? as u64);
            }
            maps.push((type_id, Heightmap::from_longs(world_height, longs)?));
        }
        Ok(Self { maps })
    }

    /// Writes the 1.21.5+ typed-list heightmap framing.
    pub fn encode(&self, w: &mut Writer) {
        w.var_i32(self.maps.len() as i32);
        for (type_id, map) in &self.maps {
            w.var_i32(*type_id as i32);
            let longs = map.longs();
            w.var_i32(longs.len() as i32);
            for &long in longs {
                w.i64(long as i64);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_bits_matches_ceillog2() {
        // 1.18+ overworld: 384 blocks tall => heights 0..=384 need 9 bits.
        assert_eq!(height_bits(384), 9);
        // Legacy: 256 tall => 0..=256 need 9 bits as well (257 values).
        assert_eq!(height_bits(256), 9);
        assert_eq!(height_bits(255), 8);
        assert_eq!(height_bits(1), 1);
    }

    #[test]
    fn index_is_xz() {
        // (x=3, z=5) => 3 + 5*16 = 83.
        assert_eq!(Heightmap::index(3, 5), 83);
    }

    #[test]
    fn set_get_round_trips() {
        let mut h = Heightmap::new(384);
        h.set(0, 0, 384);
        h.set(15, 15, 63);
        h.set(7, 9, 200);
        assert_eq!(h.get(0, 0), 384);
        assert_eq!(h.get(15, 15), 63);
        assert_eq!(h.get(7, 9), 200);
    }

    #[test]
    fn typed_list_round_trips() {
        let mut maps = Heightmaps::new();
        let mut motion = Heightmap::new(384);
        let mut surface = Heightmap::new(384);
        for x in 0..16 {
            for z in 0..16 {
                motion.set(x, z, (x * 16 + z) as u32);
                surface.set(x, z, (x + z) as u32);
            }
        }
        maps.insert(0, motion);
        maps.insert(4, surface);

        let mut w = Writer::default();
        maps.encode(&mut w);
        let bytes = w.into_vec();

        let mut r = Reader::new(&bytes);
        let decoded = Heightmaps::decode(384, &mut r).expect("decode");
        assert!(r.is_empty());
        assert_eq!(decoded, maps);
        assert_eq!(decoded.get(0).unwrap().get(3, 4), 3 * 16 + 4);
    }

    #[test]
    fn decode_rejects_wrong_long_count() {
        let expected = PackedArray::long_count(height_bits(384), COLUMNS);
        let mut w = Writer::default();
        w.var_i32(1); // one map
        w.var_i32(0); // type id
        w.var_i32((expected + 3) as i32); // lies about long count
        for _ in 0..(expected + 3) {
            w.i64(0);
        }
        let err = Heightmaps::decode(384, &mut Reader::new(&w.into_vec())).unwrap_err();
        assert!(matches!(err, WorldError::WrongLongCount { .. }));
    }
}
