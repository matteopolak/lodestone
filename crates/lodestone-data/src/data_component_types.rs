//! Public data-component-type id→identifier resolution for protocol 776.
//!
//! An item stack's wire-format component patch names each added or removed component by
//! a `minecraft:data_component_type` registry id (a VarInt). The id→name
//! mapping is generated from Mojang's own `registries.json` for 26.2, the one
//! canonical internal version, so it lives here in this data crate
//! rather than in `lodestone-v26-2` — it is a game-data census,
//! not wire-format code.

pub use crate::generated_data_component_types::DATA_COMPONENT_TYPE_COUNT;
use crate::generated_data_component_types::DATA_COMPONENT_TYPE_NAMES;

/// A validated entry in the 26.2 data-component-type registry.
///
/// Item-patch codecs construct this at their wire boundary before using the
/// generated census. A custom, future, or malformed raw id deliberately does
/// not become this type: its payload cannot be generically skipped, so the
/// caller must report a partial patch instead of assigning it a built-in name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataComponentTypeId(i32);

impl DataComponentTypeId {
    /// Validates a raw registry id at a wire or import boundary.
    #[must_use]
    pub const fn new(raw: i32) -> Option<Self> {
        if raw >= 0 && raw < DATA_COMPONENT_TYPE_COUNT as i32 {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// The registry id used by the version-specific wire codec.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }
}

/// Resolves a validated data-component-type registry id to its canonical
/// `minecraft:*` identifier.
///
/// Raw values are validated with [`DataComponentTypeId::new`] before reaching
/// this total lookup. That leaves custom and future values as explicit misses
/// at the packet boundary rather than silently naming a neighboring built-in
/// component.
#[must_use]
pub fn component_type_name(id: DataComponentTypeId) -> &'static str {
    DATA_COMPONENT_TYPE_NAMES[id.raw() as usize]
}

/// Resolves a canonical `minecraft:*` identifier to its data-component-type
/// registry id for protocol 776.
///
/// This reverse lookup is used only by outbound item-patch writers. Unknown or
/// custom names stay misses, so an encoder never invents a built-in id for
/// them.
#[must_use]
pub fn component_type_id(name: &str) -> Option<DataComponentTypeId> {
    DATA_COMPONENT_TYPE_NAMES
        .iter()
        .position(|&candidate| candidate == name)
        .and_then(|index| i32::try_from(index).ok())
        .and_then(DataComponentTypeId::new)
}
