//! Folding a protocol 5 metadata list into the version-free
//! [`EntityMetadataUpdate`].
//!
//! # Only the era-wide indices are interpreted, and that is a decision
//!
//! A data-watcher index means nothing on its own: index 12 is a baby flag on
//! a zombie and a saddle flag on a pig. Interpreting a per-mob index
//! therefore requires knowing the entity's type, which the standalone
//! `entity_metadata` packet does not carry — the adapter has to remember it
//! from the spawn packet.
//!
//! This module interprets only the indices that mean the same thing for every
//! entity in the era, and leaves the per-mob ones alone. A wrong guess here
//! is not a missing feature but a *false* one: a saddle flag reported as a
//! baby flag renders a small pig, and nothing logs anything. The per-mob
//! tables are a separate, larger piece of work whose evidence must be
//! per-mob.
//!
//! # The indices, and why each number is what it is
//!
//! Every one below was read off a real 1.7.10 server, by summoning a mob with
//! distinctive values in its spawn tag and reading the numbers back out of the
//! spawn and metadata packets:
//!
//! | index | type | meaning | how it was pinned |
//! |---|---|---|---|
//! | 0 | byte | shared entity flags | a zombie summoned on fire *and* under invisibility reported `33` = `0x01 | 0x20`, so both bits are confirmed at once |
//! | 1 | short | air supply | reported `300`, the era's full-air value |
//! | 6 | float | health | summoned with `Health:5.0` and reported `5.0`; then watched count 20, 19, 18, 17 as the mob burned |
//! | 10 | string | custom name | summoned with a custom name and reported that exact string |
//! | 11 | byte | custom name visible | summoned visible and reported `1` |
//!
//! **Index 10 is the one worth staring at.** The 1.8 era puts the custom name
//! at index 2. Copying the neighbour's number would have read a
//! potion-effect field as a name — silently, since both indices exist on
//! every living entity — and it is exactly the collision trap the 1.8 era's
//! own metadata module warns about. The number here comes from the wire, not
//! from the sibling.
//!
//! Index 7 (potion effect colour, int) and 8 (potion effect ambient, byte)
//! were observed and are deliberately not folded: the canonical update has no
//! carrier for a packed effect colour, and inventing one from a colour would
//! be a guess at which effect produced it.

use lodestone_model::{EntityMetadataUpdate, Reported, Text};

use crate::packets::metadata::{EntityMetadata, MetadataValue};

/// Shared entity flags.
const INDEX_FLAGS: u8 = 0;
/// Air supply, in ticks.
const INDEX_AIR: u8 = 1;
/// Living-entity health.
const INDEX_HEALTH: u8 = 6;
/// Custom name. **Not** index 2, which is where the next era moved it.
const INDEX_CUSTOM_NAME: u8 = 10;
/// Whether the custom name renders above the entity.
const INDEX_CUSTOM_NAME_VISIBLE: u8 = 11;

/// Folds a metadata list into the version-free update.
///
/// Only indices present in the list are reported, which matters for the
/// incremental `entity_metadata` packet: it carries **only changed values**
/// (measured — a burning mob's health updates arrive as a one-entry list), so
/// an absent field must not overwrite a known one.
#[must_use]
pub fn fold(metadata: &EntityMetadata) -> EntityMetadataUpdate {
    let mut update = EntityMetadataUpdate::default();
    for entry in &metadata.0 {
        match (entry.key, &entry.value) {
            (INDEX_FLAGS, MetadataValue::Byte(bits)) => {
                update.flags = Some(*bits as u8);
            }
            (INDEX_AIR, MetadataValue::Short(air)) => {
                update.air_supply = Some(i32::from(*air));
            }
            (INDEX_HEALTH, MetadataValue::Float(health)) => {
                update.health = Some(*health);
            }
            (INDEX_CUSTOM_NAME, MetadataValue::String(name)) => {
                // An empty string is the era's explicit clear, not an absent
                // field: the server sends `""` to remove a name. Reporting it
                // as `Reported(None)` is what lets a consumer distinguish
                // "cleared" from "unchanged".
                update.custom_name = Reported::Reported(if name.is_empty() {
                    None
                } else {
                    Some(Text::literal(name.clone()))
                });
            }
            (INDEX_CUSTOM_NAME_VISIBLE, MetadataValue::Byte(visible)) => {
                update.custom_name_visible = Some(*visible != 0);
            }
            _ => {}
        }
    }
    update
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packets::metadata::MetadataEntry;

    fn list(entries: Vec<MetadataEntry>) -> EntityMetadata {
        EntityMetadata(entries)
    }

    #[test]
    fn folds_the_indices_the_wire_oracle_pinned() {
        let update = fold(&list(vec![
            MetadataEntry {
                key: 0,
                value: MetadataValue::Byte(33),
            },
            MetadataEntry {
                key: 1,
                value: MetadataValue::Short(300),
            },
            MetadataEntry {
                key: 6,
                value: MetadataValue::Float(5.0),
            },
            MetadataEntry {
                key: 10,
                value: MetadataValue::String("Xyzzy".to_owned()),
            },
            MetadataEntry {
                key: 11,
                value: MetadataValue::Byte(1),
            },
        ]));
        // The values are the ones a real server reported for a zombie
        // summoned on fire, invisible, with five health and a custom name.
        assert_eq!(update.flags, Some(33));
        assert_eq!(update.air_supply, Some(300));
        assert_eq!(update.health, Some(5.0));
        assert_eq!(
            update.custom_name,
            Reported::Reported(Some(Text::literal("Xyzzy")))
        );
        assert_eq!(update.custom_name_visible, Some(true));
    }

    #[test]
    fn an_empty_name_is_an_explicit_clear_not_an_absence() {
        let update = fold(&list(vec![MetadataEntry {
            key: 10,
            value: MetadataValue::String(String::new()),
        }]));
        assert_eq!(update.custom_name, Reported::Reported(None));
    }

    #[test]
    fn an_absent_index_is_left_unreported() {
        let update = fold(&list(Vec::new()));
        assert_eq!(update.custom_name, Reported::Unreported);
        assert_eq!(update.health, None);
        assert_eq!(update.flags, None);
    }

    #[test]
    fn index_two_is_not_read_as_a_custom_name() {
        // The 1.8 era's custom-name index. At protocol 5 index 2 is not a
        // name, so folding it as one would invent a name for every entity
        // that has the field at all.
        let update = fold(&list(vec![MetadataEntry {
            key: 2,
            value: MetadataValue::String("wrong-era".to_owned()),
        }]));
        assert_eq!(update.custom_name, Reported::Unreported);
    }
}
