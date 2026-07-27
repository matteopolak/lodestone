//! Fixed-length array of small unsigned integers packed into `u64` cells.
//!
//! This mirrors the storage vanilla calls `SimpleBitStorage`. Each entry uses a
//! fixed `bits` width. Entries are packed low-bits-first into `u64` cells and,
//! critically, **never straddle a cell boundary**: a cell holds exactly
//! `floor(64 / bits)` entries and any leftover high bits are padding. This is
//! the layout Minecraft has used on the wire and on disk since 1.16.

use crate::{Result, WorldError};

/// A packed array of `len` entries, each `bits` wide (`1..=32`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedArray {
    bits: u32,
    len: usize,
    values_per_long: usize,
    data: Vec<u64>,
}

impl PackedArray {
    /// Largest bits-per-entry this storage supports. Matches vanilla's cap and
    /// bounds worst-case allocations from untrusted input.
    pub const MAX_BITS: u32 = 32;

    /// Number of `u64` cells required to hold `len` entries at `bits` width.
    #[must_use]
    pub const fn long_count(bits: u32, len: usize) -> usize {
        let per = (64 / bits) as usize;
        len.div_ceil(per)
    }

    /// Creates a zero-filled packed array.
    ///
    /// # Panics
    /// Panics if `bits` is `0` or greater than [`PackedArray::MAX_BITS`]; the
    /// zero-bit case is represented by the container's single-value strategy,
    /// not by a `PackedArray`.
    #[must_use]
    pub fn new(bits: u32, len: usize) -> Self {
        assert!(
            (1..=Self::MAX_BITS).contains(&bits),
            "bits must be in 1..={}",
            Self::MAX_BITS
        );
        let values_per_long = (64 / bits) as usize;
        Self {
            bits,
            len,
            values_per_long,
            data: vec![0; Self::long_count(bits, len)],
        }
    }

    /// Wraps existing packed longs, validating the length against the layout.
    ///
    /// This is the entry point a size-classed buffer pool would use to hand a
    /// recycled `Vec<u64>` back into a container without reallocating.
    ///
    /// # Errors
    /// Returns [`WorldError`] if `bits` is out of range or `data` does not have
    /// exactly [`PackedArray::long_count`] elements.
    pub fn from_longs(bits: u32, len: usize, data: Vec<u64>) -> Result<Self> {
        if !(1..=Self::MAX_BITS).contains(&bits) {
            return Err(WorldError::InvalidBits {
                bits,
                entry_count: len,
            });
        }
        let expected = Self::long_count(bits, len);
        if data.len() != expected {
            return Err(WorldError::WrongLongCount {
                expected,
                actual: data.len(),
            });
        }
        Ok(Self {
            bits,
            len,
            values_per_long: (64 / bits) as usize,
            data,
        })
    }

    /// Packs `values` into a fresh array of the given `bits` width.
    ///
    /// # Panics
    /// Panics if `bits` is out of range or any value does not fit in `bits`.
    #[must_use]
    pub fn from_values(bits: u32, values: &[u32]) -> Self {
        let mut array = Self::new(bits, values.len());
        for (i, &v) in values.iter().enumerate() {
            array.set(i, v);
        }
        array
    }

    /// Bits-per-entry width.
    #[must_use]
    pub const fn bits(&self) -> u32 {
        self.bits
    }

    /// Number of entries.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the array holds no entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The packed backing longs.
    #[must_use]
    pub fn longs(&self) -> &[u64] {
        &self.data
    }

    const fn mask(&self) -> u64 {
        // bits <= 32, so this never overflows.
        (1u64 << self.bits) - 1
    }

    /// Returns the entry at `index`.
    ///
    /// # Panics
    /// Panics if `index >= len`.
    #[must_use]
    pub fn get(&self, index: usize) -> u32 {
        assert!(index < self.len, "index {index} out of bounds {}", self.len);
        let cell = index / self.values_per_long;
        let offset = (index % self.values_per_long) as u32 * self.bits;
        ((self.data[cell] >> offset) & self.mask()) as u32
    }

    /// Sets the entry at `index` to `value`.
    ///
    /// # Panics
    /// Panics if `index >= len` or `value` does not fit in `bits`.
    pub fn set(&mut self, index: usize, value: u32) {
        assert!(index < self.len, "index {index} out of bounds {}", self.len);
        let mask = self.mask();
        assert!(
            u64::from(value) <= mask,
            "value {value} exceeds {} bits",
            self.bits
        );
        let cell = index / self.values_per_long;
        let offset = (index % self.values_per_long) as u32 * self.bits;
        let cleared = self.data[cell] & !(mask << offset);
        self.data[cell] = cleared | (u64::from(value) << offset);
    }

    /// Iterates every entry in index order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.len).map(move |i| self.get(i))
    }

    /// Heap bytes owned by this array (the backing `Vec<u64>`).
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.data.capacity() * core::mem::size_of::<u64>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_five_bit_packing_does_not_straddle_longs() {
        // 5 bits => floor(64/5) = 12 entries per long, leaving 4 padding bits.
        // Two longs hold 24 entries; entry 12 starts a fresh long, NOT bit 60
        // of the first long. Build the expected longs by hand from the spec.
        // Values 0..=23, all fit in 5 bits.
        let values: Vec<u32> = (0..24u32).collect();
        assert!(values.iter().all(|&v| v < 32));

        let expected0 = hand_pack(5, &values[0..12]);
        let expected1 = hand_pack(5, &values[12..24]);

        let packed = PackedArray::from_values(5, &values);
        assert_eq!(
            packed.longs().len(),
            2,
            "24 entries at 12 per long => 2 longs"
        );
        assert_eq!(packed.longs()[0], expected0, "first long mismatch");
        assert_eq!(packed.longs()[1], expected1, "second long mismatch");

        // Round-trip every entry.
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(packed.get(i), v, "entry {i}");
        }
    }

    /// Hand-pack up to `64/bits` values into a single long, low bits first.
    fn hand_pack(bits: u32, values: &[u32]) -> u64 {
        let mut long = 0u64;
        for (i, &v) in values.iter().enumerate() {
            long |= u64::from(v) << (i as u32 * bits);
        }
        long
    }

    #[test]
    fn four_bit_packing_divides_evenly() {
        // 4 bits => exactly 16 per long, no padding. 4096 entries => 256 longs.
        let values: Vec<u32> = (0..4096u32).map(|i| i % 16).collect();
        let packed = PackedArray::from_values(4, &values);
        assert_eq!(packed.longs().len(), 256);
        assert_eq!(PackedArray::long_count(4, 4096), 256);
        for i in 0..4096 {
            assert_eq!(packed.get(i), (i as u32) % 16);
        }
    }

    #[test]
    fn from_longs_rejects_wrong_length() {
        // 5 bits, 24 entries => needs 2 longs.
        let err = PackedArray::from_longs(5, 24, vec![0; 3]).unwrap_err();
        assert_eq!(
            err,
            WorldError::WrongLongCount {
                expected: 2,
                actual: 3
            }
        );
        assert!(PackedArray::from_longs(5, 24, vec![0; 2]).is_ok());
    }

    #[test]
    fn from_longs_rejects_out_of_range_bits() {
        assert!(matches!(
            PackedArray::from_longs(0, 10, vec![]),
            Err(WorldError::InvalidBits { .. })
        ));
        assert!(matches!(
            PackedArray::from_longs(33, 10, vec![0; 10]),
            Err(WorldError::InvalidBits { .. })
        ));
    }

    #[test]
    fn set_then_get_round_trips_deterministic_fill() {
        // Deterministic pseudo-random fill (seeded LCG) at an odd width.
        let mut state = 0x1234_5678u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 11) & 0x7f // 7-bit values
        };
        let values: Vec<u32> = (0..4096).map(|_| next()).collect();
        let packed = PackedArray::from_values(7, &values);
        assert_eq!(packed.longs().len(), PackedArray::long_count(7, 4096));
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(packed.get(i), v, "entry {i}");
        }
        assert!(packed.iter().eq(values.iter().copied()));
    }
}
