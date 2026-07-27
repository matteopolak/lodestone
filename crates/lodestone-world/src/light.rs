//! Sky- and block-light storage with uniform-section elision.
//!
//! Light is, naively, the single largest consumer of chunk memory. Each section
//! stores 4096 nibbles (2048 bytes) *per light type*, and there are two types
//! (sky and block), so a full 24-section column would cost
//! `2048 * 2 * 24 = 98304` bytes of light alone — more than its blocks and
//! biomes combined. The saving is elision: underground sections are uniformly
//! zero block light, and sections above the terrain are uniformly full (15) sky
//! light. A uniform section is stored as a one-byte tag, not a 2 KiB array.
//!
//! This mirrors vanilla's `DataLayer`, whose backing byte array is `null` while
//! the layer is uniform (`fill(v)` drops the array and remembers a default),
//! and the light-update packet, which carries a present-section mask, an
//! empty-section mask, and only the arrays for genuinely non-uniform sections.

use lodestone_core::{Reader, Writer};
use std::sync::Arc;

use crate::{Result, WorldError};

/// Bytes in a full nibble array: 4096 nibbles at 2 per byte.
const LIGHT_ARRAY_BYTES: usize = 2048;

/// A dense array of 4096 four-bit light values, two nibbles per byte.
///
/// Entries are addressed either by flat index (`0..4096`) or by local
/// coordinates through [`NibbleArray::index`], which uses vanilla's YZX order
/// (`index = y << 8 | z << 4 | x`). The low nibble of byte `i` holds entry `2i`
/// and the high nibble holds entry `2i + 1`, matching `DataLayer`.
///
/// The 2 KiB backing store is held behind an [`Arc`] so that a light snapshot
/// handed to a mesher clones in O(1) (a refcount bump, not a 2 KiB copy) and a
/// later relight forks it copy-on-write: [`set`](NibbleArray::set) calls
/// [`Arc::make_mut`], so a writer whose array is still shared with a snapshot
/// forks a private copy while the snapshot keeps the old values. This is the
/// light-side equivalent of the per-section `Arc<ChunkSection>` block snapshot,
/// and it is why a block change can invalidate one section's light without
/// deep-copying every neighbour a mesher is holding.
#[derive(Clone, PartialEq, Eq)]
pub struct NibbleArray {
    bytes: Arc<[u8; LIGHT_ARRAY_BYTES]>,
}

impl std::fmt::Debug for NibbleArray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NibbleArray")
            .field("uniform", &self.uniform_value())
            .finish()
    }
}

impl NibbleArray {
    /// Number of entries in a nibble array.
    pub const LEN: usize = 4096;

    /// Creates an array with every nibble set to `value & 0x0F`.
    #[must_use]
    pub fn filled(value: u8) -> Self {
        let byte = pack_nibble(value);
        Self {
            bytes: Arc::new([byte; LIGHT_ARRAY_BYTES]),
        }
    }

    /// Builds an array from exactly 2048 raw bytes.
    ///
    /// # Errors
    /// Returns [`WorldError::InvalidLightArrayLength`] if `bytes` is not 2048
    /// bytes long.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; LIGHT_ARRAY_BYTES] = bytes
            .try_into()
            .map_err(|_| WorldError::InvalidLightArrayLength(bytes.len()))?;
        Ok(Self {
            bytes: Arc::new(arr),
        })
    }

    /// The raw 2048-byte backing store.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; LIGHT_ARRAY_BYTES] {
        &self.bytes
    }

    /// Whether two arrays share the same backing allocation (a copy-on-write
    /// snapshot that has not yet been forked). Used by tests to prove that a
    /// relight of one section does not deep-copy an unaffected snapshot.
    #[must_use]
    #[doc(hidden)]
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.bytes, &other.bytes)
    }

    /// Flat entry index for local coordinates, using vanilla's YZX order.
    ///
    /// # Panics
    /// Panics if any coordinate is outside `0..16`.
    #[must_use]
    pub fn index(x: usize, y: usize, z: usize) -> usize {
        assert!(x < 16 && y < 16 && z < 16, "coordinate out of range");
        y << 8 | z << 4 | x
    }

    /// Reads the nibble at flat `index` (`0..4096`).
    ///
    /// # Panics
    /// Panics if `index >= 4096`.
    #[must_use]
    pub fn get(&self, index: usize) -> u8 {
        assert!(index < Self::LEN, "light index out of range");
        let byte = self.bytes[index >> 1];
        (byte >> (4 * (index & 1))) & 0x0F
    }

    /// Writes `value & 0x0F` to the nibble at flat `index` (`0..4096`).
    ///
    /// Forks the backing store copy-on-write when it is shared: if a snapshot
    /// still holds a clone, this materialises a private copy and mutates that,
    /// leaving the snapshot's values untouched.
    ///
    /// # Panics
    /// Panics if `index >= 4096`.
    pub fn set(&mut self, index: usize, value: u8) {
        assert!(index < Self::LEN, "light index out of range");
        let shift = 4 * (index & 1);
        let bytes = Arc::make_mut(&mut self.bytes);
        let slot = &mut bytes[index >> 1];
        *slot = (*slot & !(0x0F << shift)) | ((value & 0x0F) << shift);
    }

    /// Returns the shared value if every nibble is identical, else `None`.
    #[must_use]
    pub fn uniform_value(&self) -> Option<u8> {
        let first = self.bytes[0];
        if first >> 4 != first & 0x0F {
            return None;
        }
        if self.bytes.iter().all(|&b| b == first) {
            Some(first & 0x0F)
        } else {
            None
        }
    }

    /// Heap bytes owned by this array (always 2048).
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        LIGHT_ARRAY_BYTES
    }
}

/// Replicates a 4-bit value into both nibbles of a byte.
fn pack_nibble(value: u8) -> u8 {
    let v = value & 0x0F;
    v | (v << 4)
}

/// The light state of one section for one light type.
///
/// Uniform sections cost a single tag byte; only genuinely varied sections hold
/// a [`NibbleArray`]. [`LightData::Missing`] means the section carried no data
/// in the update at all, so its value is implied by the client's existing state
/// rather than being all-zero.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LightData {
    /// The section was absent from the update; its value is implied/unknown.
    #[default]
    Missing,
    /// Every nibble is this value (`0..=15`); no array is allocated.
    Uniform(u8),
    /// Explicit per-block light values.
    Values(NibbleArray),
}

impl LightData {
    /// Reads the light value at flat `index`, or `None` when [`Missing`].
    ///
    /// [`Missing`]: LightData::Missing
    #[must_use]
    pub fn get(&self, index: usize) -> Option<u8> {
        match self {
            LightData::Missing => None,
            LightData::Uniform(v) => Some(*v),
            LightData::Values(arr) => Some(arr.get(index)),
        }
    }

    /// Heap bytes owned by this section's light.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        match self {
            LightData::Missing | LightData::Uniform(_) => 0,
            LightData::Values(arr) => arr.heap_bytes(),
        }
    }

    /// Normalises a decoded array into [`Uniform`] when every nibble matches,
    /// collapsing common full-bright and fully-dark sections to a tag.
    ///
    /// [`Uniform`]: LightData::Uniform
    fn from_array(arr: NibbleArray) -> Self {
        match arr.uniform_value() {
            Some(v) => LightData::Uniform(v),
            None => LightData::Values(arr),
        }
    }

    /// Writes one light value at flat `index`, materialising a per-block array on
    /// first divergence from a uniform (or missing) section.
    ///
    /// A [`Uniform`](LightData::Uniform) section whose fill already equals `value`
    /// stays a tag; otherwise it expands to a [`Values`](LightData::Values) array
    /// seeded from the fill. A [`Missing`](LightData::Missing) section is treated
    /// as newly zero-initialised — the caller is expected to have established the
    /// section (e.g. via a full relight) before poking individual nibbles. When
    /// the backing array is shared with a snapshot the write forks copy-on-write
    /// via [`NibbleArray::set`].
    pub fn set(&mut self, index: usize, value: u8) {
        match self {
            LightData::Values(arr) => arr.set(index, value),
            LightData::Uniform(v) if *v == (value & 0x0F) => {}
            LightData::Uniform(v) => {
                let mut arr = NibbleArray::filled(*v);
                arr.set(index, value);
                *self = LightData::Values(arr);
            }
            LightData::Missing => {
                let mut arr = NibbleArray::filled(0);
                arr.set(index, value);
                *self = LightData::from_array(arr);
            }
        }
    }
}

/// A lock-free, copy-on-write snapshot of one section's sky and block light.
///
/// Returned by [`World::section_light`](crate::World::section_light) as the
/// light-side companion to the block-side [`Arc<ChunkSection>`] snapshot. Both
/// light layers clone in O(1) — [`LightData::Missing`]/[`Uniform`] are tags and
/// [`Values`] shares its array through [`NibbleArray`]'s `Arc` — so a mesher can
/// hold a whole neighbourhood's light across a mesh while chunk streaming and
/// relighting continue; any later relight of a section forks its array
/// copy-on-write, leaving the snapshot's values intact.
///
/// A section snapshot is available even when the block section is elided (all
/// air): air still has sky light, and a face that meshes against air must sample
/// that light or it renders black. Gating light on block presence would
/// reintroduce exactly that trap, so this is deliberately independent of
/// [`World::section`](crate::World::section) returning `Some`.
///
/// [`Uniform`]: LightData::Uniform
/// [`Values`]: LightData::Values
/// [`Arc<ChunkSection>`]: crate::ChunkSection
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionLight {
    /// Sky light for this section.
    pub sky: LightData,
    /// Block light for this section.
    pub block: LightData,
}

/// Sky and block light for a whole column, spanning `section_count + 2` light
/// sections (vanilla lights one section below and one above the build range).
///
/// Light section `0` is the section immediately below the world's lowest block
/// section; index `i` corresponds to world section `i - 1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnLight {
    sky: Vec<LightData>,
    block: Vec<LightData>,
}

impl ColumnLight {
    /// Creates an all-[`Missing`](LightData::Missing) light column for a world of
    /// `section_count` block sections (so `section_count + 2` light sections).
    #[must_use]
    pub fn new(section_count: usize) -> Self {
        let light_sections = section_count + 2;
        Self {
            sky: vec![LightData::Missing; light_sections],
            block: vec![LightData::Missing; light_sections],
        }
    }

    /// Number of light sections (`section_count + 2`).
    #[must_use]
    pub fn light_section_count(&self) -> usize {
        self.sky.len()
    }

    /// Sky light for light section `i` (`0` is below the world).
    #[must_use]
    pub fn sky(&self, i: usize) -> &LightData {
        &self.sky[i]
    }

    /// Block light for light section `i` (`0` is below the world).
    #[must_use]
    pub fn block(&self, i: usize) -> &LightData {
        &self.block[i]
    }

    /// Mutable sky light for light section `i`.
    pub fn sky_mut(&mut self, i: usize) -> &mut LightData {
        &mut self.sky[i]
    }

    /// Mutable block light for light section `i`.
    pub fn block_mut(&mut self, i: usize) -> &mut LightData {
        &mut self.block[i]
    }

    /// An O(1) copy-on-write snapshot of light section `i` (`0` is below the
    /// world), pairing its sky and block light for a mesher.
    ///
    /// Cloning is a refcount bump per [`Values`](LightData::Values) layer and a
    /// trivial copy for tags, so this is cheap enough to call across a whole
    /// section neighbourhood. See [`SectionLight`] for why it is offered
    /// independently of whether the block section is elided.
    ///
    /// # Panics
    /// Panics if `i` is not a valid light section.
    #[must_use]
    pub fn section_light(&self, i: usize) -> SectionLight {
        SectionLight {
            sky: self.sky[i].clone(),
            block: self.block[i].clone(),
        }
    }

    /// Writes one sky-light value at section-local flat `index` in light section
    /// `i`, forking that section's array copy-on-write if it is shared.
    ///
    /// # Panics
    /// Panics if `i` is not a valid light section or `index >= 4096`.
    pub fn set_sky_light(&mut self, i: usize, index: usize, value: u8) {
        self.sky[i].set(index, value);
    }

    /// Writes one block-light value at section-local flat `index` in light
    /// section `i`, forking that section's array copy-on-write if it is shared.
    ///
    /// # Panics
    /// Panics if `i` is not a valid light section or `index >= 4096`.
    pub fn set_block_light(&mut self, i: usize, index: usize, value: u8) {
        self.block[i].set(index, value);
    }

    /// Sky light at world block-section `section_index` (`0` is the lowest block
    /// section) for section-local coordinates in `0..16`, using vanilla's YZX
    /// order.
    ///
    /// This resolves the column→section off-by-one once, in the crate that owns
    /// the layout: light section `section_index + 1` covers world block-section
    /// `section_index` (light section `0` sits below the world). Returns `None`
    /// when that section carried no data ([`LightData::Missing`]), letting a
    /// section-level adapter pick its own default — commonly full sky light above
    /// terrain, zero block light underground. The lookup is one bounds-checked
    /// index plus a nibble read with no allocation, so a mesher's per-section
    /// light view built on it stays cheap.
    ///
    /// # Panics
    /// Panics if any coordinate is outside `0..16`, or if `section_index + 1` is
    /// not a valid light section.
    #[must_use]
    pub fn section_sky_light(
        &self,
        section_index: usize,
        x: usize,
        y: usize,
        z: usize,
    ) -> Option<u8> {
        self.sky(section_index + 1).get(NibbleArray::index(x, y, z))
    }

    /// Block light at world block-section `section_index` for section-local
    /// coordinates in `0..16` (YZX order). The section-mapping and cost notes on
    /// [`section_sky_light`](Self::section_sky_light) apply identically.
    ///
    /// # Panics
    /// Panics if any coordinate is outside `0..16`, or if `section_index + 1` is
    /// not a valid light section.
    #[must_use]
    pub fn section_block_light(
        &self,
        section_index: usize,
        x: usize,
        y: usize,
        z: usize,
    ) -> Option<u8> {
        self.block(section_index + 1)
            .get(NibbleArray::index(x, y, z))
    }

    /// Heap bytes owned by all non-uniform light arrays in this column.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        let arrays: usize = self
            .sky
            .iter()
            .chain(&self.block)
            .map(LightData::heap_bytes)
            .sum();
        arrays + (self.sky.capacity() + self.block.capacity()) * size_of::<LightData>()
    }

    /// Writes the column in the light-update wire format: four section bitsets
    /// (present-sky, present-block, empty-sky, empty-block) followed by the sky
    /// and block arrays for each present section, in ascending section order.
    pub fn encode(&self, w: &mut Writer) {
        let (sky_mask, empty_sky) = self.masks(&self.sky);
        let (block_mask, empty_block) = self.masks(&self.block);
        write_bitset(w, &sky_mask, self.light_section_count());
        write_bitset(w, &block_mask, self.light_section_count());
        write_bitset(w, &empty_sky, self.light_section_count());
        write_bitset(w, &empty_block, self.light_section_count());
        write_light_list(w, &self.sky);
        write_light_list(w, &self.block);
    }

    /// Present- and empty-section masks (as bit vectors) for one light layer.
    fn masks(&self, layer: &[LightData]) -> (Vec<bool>, Vec<bool>) {
        let mut present = vec![false; layer.len()];
        let mut empty = vec![false; layer.len()];
        for (i, data) in layer.iter().enumerate() {
            match data {
                LightData::Missing => {}
                LightData::Uniform(0) => empty[i] = true,
                LightData::Uniform(_) | LightData::Values(_) => present[i] = true,
            }
        }
        (present, empty)
    }

    /// Reads a light column of `section_count` block sections from the wire.
    ///
    /// # Errors
    /// Returns [`WorldError`] if a mask flags a section outside the light range,
    /// if an update list does not supply exactly one array per present section,
    /// or if any array is not 2048 bytes.
    pub fn decode(section_count: usize, r: &mut Reader<'_>) -> Result<Self> {
        let count = section_count + 2;
        let sky_mask = read_bitset(r, count)?;
        let block_mask = read_bitset(r, count)?;
        let empty_sky = read_bitset(r, count)?;
        let empty_block = read_bitset(r, count)?;

        let sky_arrays = read_light_list(r)?;
        let block_arrays = read_light_list(r)?;

        let sky = assemble_layer(count, &sky_mask, &empty_sky, sky_arrays)?;
        let block = assemble_layer(count, &block_mask, &empty_block, block_arrays)?;
        Ok(Self { sky, block })
    }
}

/// Combines a present mask, an empty mask, and the supplied arrays into one
/// light layer, validating that exactly one array is consumed per present bit.
fn assemble_layer(
    count: usize,
    present: &[bool],
    empty: &[bool],
    arrays: Vec<NibbleArray>,
) -> Result<Vec<LightData>> {
    let present_total = present.iter().filter(|&&b| b).count();
    if present_total != arrays.len() {
        return Err(WorldError::LightUpdateCountMismatch {
            expected: present_total,
            actual: arrays.len(),
        });
    }

    let mut layer = vec![LightData::Missing; count];
    let mut arrays = arrays.into_iter();
    for i in 0..count {
        if present[i] {
            layer[i] = LightData::from_array(arrays.next().expect("count checked above"));
        } else if empty[i] {
            layer[i] = LightData::Uniform(0);
        }
    }
    Ok(layer)
}

/// Writes a bitset as vanilla `writeBitSet` does: a VarInt-prefixed little-word
/// long array with trailing all-zero words trimmed.
fn write_bitset(w: &mut Writer, bits: &[bool], _count: usize) {
    let mut words: Vec<u64> = Vec::new();
    for (i, &set) in bits.iter().enumerate() {
        if set {
            let word = i / 64;
            if word >= words.len() {
                words.resize(word + 1, 0);
            }
            words[word] |= 1u64 << (i % 64);
        }
    }
    w.var_i32(words.len() as i32);
    for word in words {
        w.i64(word as i64);
    }
}

/// Reads a `writeBitSet` long array into a bit vector of length `count`.
///
/// The declared long count is bounded by the light range, so a hostile length
/// cannot force an unbounded allocation, and any bit at or beyond `count` is
/// rejected rather than silently ignored.
fn read_bitset(r: &mut Reader<'_>, count: usize) -> Result<Vec<bool>> {
    let max_words = count.div_ceil(64);
    let declared = r.var_i32()?;
    if declared < 0 || declared as usize > max_words {
        return Err(WorldError::LightSectionOutOfRange {
            bit: declared.max(0) as usize * 64,
            count,
        });
    }
    let mut bits = vec![false; count];
    for word_index in 0..declared as usize {
        let word = r.i64()? as u64;
        for bit in 0..64 {
            if word & (1u64 << bit) != 0 {
                let section = word_index * 64 + bit;
                if section >= count {
                    return Err(WorldError::LightSectionOutOfRange {
                        bit: section,
                        count,
                    });
                }
                bits[section] = true;
            }
        }
    }
    Ok(bits)
}

/// Writes the present-section arrays of a light layer as a VarInt-counted list
/// of 2048-byte (length-prefixed) arrays, in ascending section order. Uniform
/// non-zero sections are materialised into a filled array, matching vanilla.
fn write_light_list(w: &mut Writer, layer: &[LightData]) {
    let present: Vec<&LightData> = layer
        .iter()
        .filter(|d| matches!(d, LightData::Values(_) | LightData::Uniform(1..=15)))
        .collect();
    w.var_i32(present.len() as i32);
    for data in present {
        match data {
            LightData::Values(arr) => {
                w.var_i32(LIGHT_ARRAY_BYTES as i32);
                w.bytes(arr.as_bytes());
            }
            LightData::Uniform(v) => {
                w.var_i32(LIGHT_ARRAY_BYTES as i32);
                w.bytes(&[pack_nibble(*v); LIGHT_ARRAY_BYTES]);
            }
            LightData::Missing => {
                unreachable!("filtered to Values or Uniform(1..=15)")
            }
        }
    }
}

/// Reads a VarInt-counted list of 2048-byte light arrays.
///
/// The per-array length prefix is validated to be exactly 2048 before the bytes
/// are read, so a malformed length errors rather than over-allocating.
fn read_light_list(r: &mut Reader<'_>) -> Result<Vec<NibbleArray>> {
    let count = r.var_i32()?;
    if count < 0 {
        return Err(WorldError::InvalidLightArrayLength(0));
    }
    let mut arrays = Vec::with_capacity(count.min(4096) as usize);
    for _ in 0..count {
        let len = r.var_i32()?;
        if len != LIGHT_ARRAY_BYTES as i32 {
            return Err(WorldError::InvalidLightArrayLength(len.max(0) as usize));
        }
        let bytes = r.bytes(LIGHT_ARRAY_BYTES)?;
        arrays.push(NibbleArray::from_bytes(bytes)?);
    }
    Ok(arrays)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nibble_round_trips_low_and_high() {
        let mut arr = NibbleArray::filled(0);
        arr.set(0, 5); // low nibble of byte 0
        arr.set(1, 12); // high nibble of byte 0
        arr.set(4095, 7);
        assert_eq!(arr.get(0), 5);
        assert_eq!(arr.get(1), 12);
        assert_eq!(arr.get(4095), 7);
        // Byte 0 packs entry 1 in the high nibble, entry 0 in the low nibble.
        assert_eq!(arr.as_bytes()[0], 0xC5);
    }

    #[test]
    fn nibble_index_is_yzx() {
        // Entry (x=1, y=2, z=3) = 2<<8 | 3<<4 | 1 = 512 + 48 + 1 = 561.
        assert_eq!(NibbleArray::index(1, 2, 3), 561);
    }

    #[test]
    fn uniform_detection() {
        assert_eq!(NibbleArray::filled(15).uniform_value(), Some(15));
        assert_eq!(NibbleArray::filled(0).uniform_value(), Some(0));
        let mut arr = NibbleArray::filled(15);
        arr.set(1000, 3);
        assert_eq!(arr.uniform_value(), None);
    }

    #[test]
    fn set_forks_a_shared_array_copy_on_write() {
        // A snapshot handed to a mesher must not mutate when the source is later
        // relit. Cloning shares the Arc; the first write to either side forks it.
        let mut original = NibbleArray::filled(0);
        original.set(5, 7);
        let snapshot = original.clone();
        assert!(
            original.shares_storage_with(&snapshot),
            "clone shares the backing allocation"
        );
        original.set(6, 3);
        assert!(
            !original.shares_storage_with(&snapshot),
            "the write forked the shared array"
        );
        assert_eq!(snapshot.get(6), 0, "snapshot kept the pre-write value");
        assert_eq!(original.get(6), 3);
        assert_eq!(snapshot.get(5), 7, "snapshot kept the shared prefix too");
    }

    #[test]
    fn light_data_set_promotes_uniform_and_missing() {
        // Uniform stays a tag until a value diverges, then materialises an array.
        let mut u = LightData::Uniform(15);
        u.set(0, 15); // equal to fill → stays a tag
        assert_eq!(u, LightData::Uniform(15));
        u.set(0, 3); // diverges → array seeded from the fill
        match &u {
            LightData::Values(arr) => {
                assert_eq!(arr.get(0), 3);
                assert_eq!(arr.get(1), 15, "the rest keeps the old fill");
            }
            other => panic!("expected Values, got {other:?}"),
        }
        // Missing is treated as newly zero-initialised on first poke.
        let mut m = LightData::Missing;
        m.set(2, 6);
        match &m {
            LightData::Values(arr) => {
                assert_eq!(arr.get(2), 6);
                assert_eq!(arr.get(0), 0);
            }
            other => panic!("expected Values, got {other:?}"),
        }
    }

    #[test]
    fn uniform_sections_store_nothing() {
        let mut light = ColumnLight::new(24);
        assert_eq!(light.light_section_count(), 26);
        for i in 0..light.light_section_count() {
            *light.sky_mut(i) = LightData::Uniform(15);
            *light.block_mut(i) = LightData::Uniform(0);
        }
        // No nibble arrays allocated: only the two Vec spines cost heap.
        let spine = 26 * 2 * size_of::<LightData>();
        assert_eq!(light.heap_bytes(), spine);
    }

    fn sample_column() -> ColumnLight {
        let mut light = ColumnLight::new(4); // 6 light sections
        *light.sky_mut(0) = LightData::Missing;
        *light.sky_mut(1) = LightData::Uniform(0); // empty mask
        *light.sky_mut(2) = LightData::Uniform(15); // full array on the wire
        let mut arr = NibbleArray::filled(4);
        arr.set(10, 9);
        arr.set(11, 2);
        *light.sky_mut(3) = LightData::Values(arr);
        *light.block_mut(0) = LightData::Uniform(0);
        let mut barr = NibbleArray::filled(0);
        barr.set(2048, 7);
        *light.block_mut(5) = LightData::Values(barr);
        light
    }

    #[test]
    fn wire_round_trip_preserves_every_section() {
        let light = sample_column();
        let mut w = Writer::default();
        light.encode(&mut w);
        let bytes = w.into_vec();

        let mut r = Reader::new(&bytes);
        let decoded = ColumnLight::decode(4, &mut r).expect("decode");
        assert!(r.is_empty(), "light decode left trailing bytes");

        for i in 0..light.light_section_count() {
            // Uniform(15) is sent as a full array and comes back collapsed.
            let expect_sky = match light.sky(i) {
                LightData::Missing => LightData::Missing,
                other => other.clone(),
            };
            for idx in [0usize, 10, 11, 2048, 4095] {
                assert_eq!(
                    decoded.sky(i).get(idx),
                    expect_sky.get(idx),
                    "sky section {i} idx {idx}"
                );
                assert_eq!(
                    decoded.block(i).get(idx),
                    light.block(i).get(idx),
                    "block section {i} idx {idx}"
                );
            }
        }
    }

    #[test]
    fn section_indexed_accessors_resolve_the_off_by_one() {
        let light = sample_column(); // 6 light sections, world sections 0..4
        // World section 0 -> light section 1 (sky Uniform(0), block Missing).
        assert_eq!(light.section_sky_light(0, 0, 0, 0), Some(0));
        assert_eq!(light.section_block_light(0, 0, 0, 0), None);
        // World section 1 -> light section 2 (sky Uniform(15)).
        assert_eq!(light.section_sky_light(1, 5, 6, 7), Some(15));
        // World section 2 -> light section 3 (explicit array: idx 10 = 9, 11 = 2).
        assert_eq!(light.section_sky_light(2, 10, 0, 0), Some(9));
        assert_eq!(light.section_sky_light(2, 11, 0, 0), Some(2));
        // World section 4 -> light section 5 (block array: idx 2048 = (x0,y8,z0) = 7).
        assert_eq!(light.section_block_light(4, 0, 8, 0), Some(7));
    }

    #[test]
    fn decoded_uniform_array_collapses_to_tag() {
        let mut light = ColumnLight::new(1); // 3 light sections
        *light.sky_mut(0) = LightData::Values(NibbleArray::filled(15));
        let mut w = Writer::default();
        light.encode(&mut w);
        let decoded = ColumnLight::decode(1, &mut Reader::new(&w.into_vec())).unwrap();
        assert_eq!(*decoded.sky(0), LightData::Uniform(15));
        assert_eq!(decoded.sky(0).heap_bytes(), 0);
    }

    #[test]
    fn missing_sections_stay_missing() {
        let light = ColumnLight::new(2); // 4 light sections, all Missing
        let mut w = Writer::default();
        light.encode(&mut w);
        let decoded = ColumnLight::decode(2, &mut Reader::new(&w.into_vec())).unwrap();
        for i in 0..decoded.light_section_count() {
            assert_eq!(*decoded.sky(i), LightData::Missing);
            assert_eq!(*decoded.block(i), LightData::Missing);
        }
    }

    #[test]
    fn rejects_mask_bit_out_of_range() {
        // A sky mask with bit 10 set but only 3 light sections.
        let mut w = Writer::default();
        w.var_i32(1); // one word
        w.i64(1 << 10);
        // remaining masks + lists never reached
        let err = ColumnLight::decode(1, &mut Reader::new(&w.into_vec())).unwrap_err();
        assert!(matches!(err, WorldError::LightSectionOutOfRange { .. }));
    }

    #[test]
    fn rejects_update_count_mismatch() {
        // Present mask flags section 0, but the sky list supplies zero arrays.
        let mut w = Writer::default();
        write_bitset(&mut w, &[true, false, false], 3); // sky present bit 0
        write_bitset(&mut w, &[false, false, false], 3); // block present
        write_bitset(&mut w, &[false, false, false], 3); // empty sky
        write_bitset(&mut w, &[false, false, false], 3); // empty block
        w.var_i32(0); // sky list: empty (mismatch!)
        w.var_i32(0); // block list
        let err = ColumnLight::decode(1, &mut Reader::new(&w.into_vec())).unwrap_err();
        assert!(matches!(
            err,
            WorldError::LightUpdateCountMismatch {
                expected: 1,
                actual: 0
            }
        ));
    }

    #[test]
    fn rejects_wrong_array_length() {
        let mut w = Writer::default();
        write_bitset(&mut w, &[true, false, false], 3);
        write_bitset(&mut w, &[false, false, false], 3);
        write_bitset(&mut w, &[false, false, false], 3);
        write_bitset(&mut w, &[false, false, false], 3);
        w.var_i32(1); // one sky array...
        w.var_i32(100); // ...but only 100 bytes
        w.bytes(&[0u8; 100]);
        let err = ColumnLight::decode(1, &mut Reader::new(&w.into_vec())).unwrap_err();
        assert!(matches!(err, WorldError::InvalidLightArrayLength(100)));
    }
}
