//! Version-free entity metadata.
//!
//! Entity metadata is one of the most version-divergent parts of the protocol:
//! the **index** each field sits at and the **serializer id** used to encode it
//! move between versions constantly (a 1.8 entity's "flags" byte is at a
//! different index and uses a different type tag than a 1.21 one). Hardcoding
//! any of that here would defeat the version-isolation architecture.
//!
//! So this module holds only the *version-free* pieces:
//!
//! * [`MetadataValue`] — the value *shapes* that survive across versions
//!   (byte, int, float, string, boolean, rotation, block position, …). A
//!   version crate decodes the wire's serializer ids into these.
//! * [`EntityMetadata`] — an index-keyed bag of those values.
//! * [`MetadataSchema`] — the seam a version crate implements to say *which
//!   index* a semantic field lives at. This mirrors the
//!   [`LongArrayFraming`](lodestone_world) knob pattern in `lodestone-world`:
//!   the shared crate carries the semantics, a per-version value selects the
//!   layout.
//!
//! The bit meanings inside the shared entity flags byte (on-fire, crouching,
//! sprinting, …) *are* stable and live here as [`SharedEntityFlags`]; only the
//! byte's index is version-supplied.

use lodestone_model::BlockPos;
use std::collections::BTreeMap;
use uuid::Uuid;

/// A single metadata value, in the version-free shapes that are stable across
/// protocol versions.
///
/// A version crate is responsible for turning the wire's serializer-id-tagged
/// payload into one of these, and back. New shapes are added here as new
/// versions introduce genuinely new value kinds; the enum is `non_exhaustive`
/// so that is not a breaking change for match sites that use a wildcard arm.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MetadataValue {
    /// A signed byte (also used for bitfields like the base entity flags).
    Byte(i8),
    /// A variable-length signed integer.
    Int(i32),
    /// A variable-length signed long.
    Long(i64),
    /// A single-precision float.
    Float(f32),
    /// A UTF-8 string.
    String(String),
    /// A boolean.
    Boolean(bool),
    /// An optional block position (present flag + position).
    OptBlockPos(Option<BlockPos>),
    /// A block position.
    BlockPos(BlockPos),
    /// An optional UUID.
    OptUuid(Option<Uuid>),
    /// Euler rotation as three floats (used by armor-stand poses).
    Rotations([f32; 3]),
    /// A signed integer holding a registry id whose meaning is version-defined
    /// (e.g. a pose, a block-state id, a particle id). Kept opaque here.
    RegistryId(i32),
    /// A raw, version-opaque payload the shared crate does not interpret.
    ///
    /// This is the escape hatch: a value shape a version has that nothing else
    /// does travels as bytes and is understood only by that version's crate.
    Opaque(Vec<u8>),
}

impl MetadataValue {
    /// Interprets this value as a signed byte, if it is one.
    #[must_use]
    pub fn as_byte(&self) -> Option<i8> {
        match self {
            MetadataValue::Byte(b) => Some(*b),
            _ => None,
        }
    }

    /// Interprets this value as an `i32`, widening a byte if necessary.
    #[must_use]
    pub fn as_int(&self) -> Option<i32> {
        match self {
            MetadataValue::Int(v) | MetadataValue::RegistryId(v) => Some(*v),
            MetadataValue::Byte(b) => Some(i32::from(*b)),
            _ => None,
        }
    }

    /// Interprets this value as a float, if it is one.
    #[must_use]
    pub fn as_float(&self) -> Option<f32> {
        match self {
            MetadataValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Interprets this value as a boolean, if it is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            MetadataValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }
}

/// An index-keyed bag of metadata values.
///
/// Vanilla transmits metadata as a sparse list of `(index, value)` entries and
/// applies them cumulatively; a `0xFF` index terminates the list. This bag
/// holds the accumulated state. Indices are ordered so iteration is
/// deterministic (helpful for tests and diffing).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntityMetadata {
    entries: BTreeMap<u8, MetadataValue>,
}

impl EntityMetadata {
    /// Creates an empty metadata bag.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies (inserts or overwrites) a value at `index`.
    pub fn set(&mut self, index: u8, value: MetadataValue) {
        self.entries.insert(index, value);
    }

    /// The value at `index`, if present.
    #[must_use]
    pub fn get(&self, index: u8) -> Option<&MetadataValue> {
        self.entries.get(&index)
    }

    /// Removes the value at `index`, returning it if present.
    pub fn remove(&mut self, index: u8) -> Option<MetadataValue> {
        self.entries.remove(&index)
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the bag is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates entries in ascending index order.
    pub fn iter(&self) -> impl Iterator<Item = (u8, &MetadataValue)> {
        self.entries.iter().map(|(k, v)| (*k, v))
    }

    /// The base entity flags byte, read from the version-supplied index, decoded
    /// into [`SharedEntityFlags`]. Returns `None` if absent or not a byte.
    #[must_use]
    pub fn shared_flags(&self, schema: &dyn MetadataSchema) -> Option<SharedEntityFlags> {
        self.get(schema.shared_flags_index())
            .and_then(MetadataValue::as_byte)
            .map(SharedEntityFlags::from_bits)
    }
}

/// The seam by which a version crate maps semantic fields to concrete indices.
///
/// Only the *base* entity fields are modelled here — enough to prove the seam —
/// because those are shared by every living entity. A version crate returns the
/// index its protocol uses. This is deliberately a trait (rather than a struct
/// of `u8`s) so a version can compute an index from an intra-family predicate if
/// it must, exactly like a small `LongArrayFraming`-style knob.
pub trait MetadataSchema {
    /// The index of the shared entity flags byte (`on fire / crouch / sprint /
    /// swim / invisible / glowing / elytra`).
    fn shared_flags_index(&self) -> u8;

    /// The index of the air-supply `VarInt`, if this version exposes it.
    fn air_supply_index(&self) -> Option<u8> {
        None
    }

    /// The index of the optional custom-name `Text`, if exposed.
    fn custom_name_index(&self) -> Option<u8> {
        None
    }

    /// The index of the custom-name-visible boolean, if exposed.
    fn custom_name_visible_index(&self) -> Option<u8> {
        None
    }
}

/// The bit meanings inside the shared entity flags byte. These bit positions are
/// stable across modern versions; only the byte's *index* is version-specific,
/// which is why they live in the shared crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SharedEntityFlags {
    /// The raw byte value.
    pub bits: u8,
}

impl SharedEntityFlags {
    const ON_FIRE: u8 = 0x01;
    const CROUCHING: u8 = 0x02;
    const SPRINTING: u8 = 0x08;
    const SWIMMING: u8 = 0x10;
    const INVISIBLE: u8 = 0x20;
    const GLOWING: u8 = 0x40;
    const FALL_FLYING: u8 = 0x80;

    /// Wraps a raw byte.
    #[must_use]
    pub const fn from_bits(bits: i8) -> Self {
        Self { bits: bits as u8 }
    }

    /// Whether the entity is on fire.
    #[must_use]
    pub const fn on_fire(self) -> bool {
        self.bits & Self::ON_FIRE != 0
    }

    /// Whether the entity is crouching (sneaking).
    #[must_use]
    pub const fn crouching(self) -> bool {
        self.bits & Self::CROUCHING != 0
    }

    /// Whether the entity is sprinting.
    #[must_use]
    pub const fn sprinting(self) -> bool {
        self.bits & Self::SPRINTING != 0
    }

    /// Whether the entity is swimming.
    #[must_use]
    pub const fn swimming(self) -> bool {
        self.bits & Self::SWIMMING != 0
    }

    /// Whether the entity is invisible.
    #[must_use]
    pub const fn invisible(self) -> bool {
        self.bits & Self::INVISIBLE != 0
    }

    /// Whether the entity has the glowing effect.
    #[must_use]
    pub const fn glowing(self) -> bool {
        self.bits & Self::GLOWING != 0
    }

    /// Whether the entity is fall-flying (elytra).
    #[must_use]
    pub const fn fall_flying(self) -> bool {
        self.bits & Self::FALL_FLYING != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for a version crate's schema, with 26.2-like indices. It lives
    /// in the test, not the shared crate, which is the whole point.
    struct ModernSchema;
    impl MetadataSchema for ModernSchema {
        fn shared_flags_index(&self) -> u8 {
            0
        }
        fn air_supply_index(&self) -> Option<u8> {
            Some(1)
        }
    }

    /// A stand-in for a 1.8-family schema; flags still at 0 there but the point
    /// is the shared crate never assumed an index.
    struct LegacySchema;
    impl MetadataSchema for LegacySchema {
        fn shared_flags_index(&self) -> u8 {
            0
        }
    }

    #[test]
    fn flags_decode_via_schema() {
        let mut md = EntityMetadata::new();
        md.set(0, MetadataValue::Byte((0x08 | 0x01) as i8)); // sprinting + on fire
        let flags = md.shared_flags(&ModernSchema).unwrap();
        assert!(flags.sprinting());
        assert!(flags.on_fire());
        assert!(!flags.crouching());
    }

    #[test]
    fn same_bag_decodes_under_two_schemas() {
        let mut md = EntityMetadata::new();
        md.set(0, MetadataValue::Byte(0x40)); // glowing
        assert!(md.shared_flags(&ModernSchema).unwrap().glowing());
        assert!(md.shared_flags(&LegacySchema).unwrap().glowing());
    }

    #[test]
    fn cumulative_apply_and_override() {
        let mut md = EntityMetadata::new();
        md.set(5, MetadataValue::Int(10));
        md.set(5, MetadataValue::Int(20));
        assert_eq!(md.get(5).and_then(MetadataValue::as_int), Some(20));
        assert_eq!(md.len(), 1);
    }

    #[test]
    fn value_accessors() {
        assert_eq!(MetadataValue::Byte(3).as_int(), Some(3));
        assert_eq!(MetadataValue::Float(1.5).as_float(), Some(1.5));
        assert_eq!(MetadataValue::Boolean(true).as_bool(), Some(true));
        assert_eq!(MetadataValue::String("x".into()).as_int(), None);
    }

    #[test]
    fn iteration_is_index_ordered() {
        let mut md = EntityMetadata::new();
        md.set(9, MetadataValue::Int(9));
        md.set(1, MetadataValue::Int(1));
        md.set(4, MetadataValue::Int(4));
        let idx: Vec<u8> = md.iter().map(|(i, _)| i).collect();
        assert_eq!(idx, vec![1, 4, 9]);
    }
}
