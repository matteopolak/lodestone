//! Public attribute id→name resolution for protocol 776.
//!
//! `update_attributes` carries each attribute as a network **registry id** (a
//! varint), not its identifier. The id→name mapping is generated from Mojang's
//! own `registries.json` for 26.2, the one canonical internal version,
//! so it lives here in this data crate rather than in `lodestone-v26-2` —
//! it is a game-data census, not wire-format code, so a version-free
//! consumer can read it with no protocol dependency. The generated array is
//! the single source of truth; this module is only the thin bounds-checked
//! accessor over it.

pub use crate::generated_attribute_types::ATTRIBUTE_COUNT;
use crate::generated_attribute_types::ATTRIBUTE_NAMES;

/// A validated entry in the 26.2 attribute registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttributeId(i32);

impl AttributeId {
    /// Validates a raw network registry id at a wire or import boundary.
    #[must_use]
    pub const fn new(raw: i32) -> Option<Self> {
        if raw >= 0 && raw < ATTRIBUTE_COUNT as i32 {
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

/// Resolves a validated network attribute id to its canonical `minecraft:*`
/// identifier.
///
/// Raw values are validated by [`AttributeId::new`] before they enter this
/// total lookup, so a malformed or future-version id remains an explicit miss
/// at the boundary rather than a silently wrong attribute.
#[must_use]
pub fn attribute_name(id: AttributeId) -> &'static str {
    ATTRIBUTE_NAMES[id.raw() as usize]
}

/// Resolves a canonical `minecraft:*` identifier to its network attribute id
/// — the reverse of [`attribute_name`]. Nothing needed this until an encoder
/// existed on the server side (`update_attributes` was decode-only), so this
/// is the first caller.
///
/// `name` must be the full namespaced identifier (`"minecraft:armor"`, not
/// bare `"armor"`), matching what [`attribute_name`] returns. A linear scan
/// over `ATTRIBUTE_COUNT` (40) entries rather than a generated reverse table:
/// this is called once per attribute per packet, not per tick.
#[must_use]
pub fn attribute_id(name: &str) -> Option<AttributeId> {
    ATTRIBUTE_NAMES
        .iter()
        .position(|&candidate| candidate == name)
        .and_then(|index| i32::try_from(index).ok())
        .and_then(AttributeId::new)
}
