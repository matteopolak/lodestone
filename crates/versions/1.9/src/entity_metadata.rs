//! Protocol 110..=340 entity-metadata translation.
//!
//! The wire list is already decoded losslessly by [`crate::packets::metadata`].
//! This module is the intentionally small semantic census for the fields that
//! can be translated without knowing the concrete entity class:
//!
//! | field | index | wire value | canonical output |
//! | --- | ---: | --- | --- |
//! | shared entity flags | 0 | signed byte | `EntityMetadataUpdate::flags` |
//!
//! The base flags byte is universal across this era and its bit positions are
//! the same as the canonical shared flags (`on fire`, crouching, sprinting,
//! invisible, glowing and fall-flying). Class-specific indices, health,
//! custom-name serializers, pose, and living/mob flags are not emitted here:
//! their historical index/type tables differ between the four protocols and
//! the current family evidence does not establish a safe class registry for
//! an incremental packet. The packet decoder still retains those entries, and
//! rejects an unknown serializer instead of silently consuming the next field.

use lodestone_model::EntityMetadataUpdate;

use crate::packets::metadata::{EntityMetadata, MetadataValue};

/// Universal shared-entity flags index for protocols 110, 210, 316 and 340.
pub const SHARED_FLAGS_INDEX: u8 = 0;

/// Folds the supported v1-9 metadata census into the version-free event model.
///
/// The metadata packet is incremental, so an absent entry means "unchanged";
/// it must not be represented as `Some(0)`. A wrong serializer at index zero
/// is likewise ignored rather than reinterpreted as a flag byte.
#[must_use]
pub fn fold(metadata: &EntityMetadata) -> EntityMetadataUpdate {
    let mut update = EntityMetadataUpdate::default();
    if let Some(MetadataValue::Byte(bits)) = metadata
        .0
        .iter()
        .find(|entry| entry.key == SHARED_FLAGS_INDEX)
        .map(|entry| &entry.value)
    {
        update.flags = Some(*bits as u8);
    }
    update
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packets::metadata::{MetadataEntry, MetadataValue};

    #[test]
    fn shared_flags_are_preserved_as_a_literal_canonical_bitset() {
        let metadata = EntityMetadata(vec![MetadataEntry {
            key: SHARED_FLAGS_INDEX,
            value: MetadataValue::Byte(-55),
        }]);
        assert_eq!(fold(&metadata).flags, Some(0xC9));
    }

    #[test]
    fn absent_and_wrongly_typed_flags_are_not_reported_as_clears() {
        assert_eq!(fold(&EntityMetadata::default()).flags, None);
        let metadata = EntityMetadata(vec![MetadataEntry {
            key: SHARED_FLAGS_INDEX,
            value: MetadataValue::VarInt(1),
        }]);
        assert_eq!(fold(&metadata).flags, None);
    }
}
