//! Historical block/item registry bridges for the three protocols in this
//! family.
//!
//! The wire ids in `block_action` and `entity_equipment` are registration ids,
//! not the canonical 26.2 ids owned by `lodestone-data`. These generated tables
//! preserve each release's own id order and are selected by negotiated protocol
//! before a resource key reaches the shared event model.

#[path = "generated/legacy_registries.rs"]
mod generated;

/// Resolves a historical item registry id to its namespaced key.
pub(crate) fn item_name(protocol: i32, id: i32) -> Option<&'static str> {
    let id = usize::try_from(id).ok()?;
    match protocol {
        crate::PROTOCOL_1_14_4 => generated::resolve(&generated::ITEMS_498, id),
        crate::PROTOCOL_1_15_2 => generated::resolve(&generated::ITEMS_578, id),
        crate::PROTOCOL_1_16_5 => generated::resolve(&generated::ITEMS_754, id),
        _ => None,
    }
}

/// Resolves a namespaced item key to this protocol's historical registration id.
pub(crate) fn item_id(protocol: i32, name: &str) -> Option<i32> {
    let table = item_table(protocol)?;
    table
        .iter()
        .position(|&entry| generated::NAMES[usize::from(entry)] == name)
        .and_then(|id| i32::try_from(id).ok())
}

fn item_table(protocol: i32) -> Option<&'static [u16]> {
    Some(match protocol {
        crate::PROTOCOL_1_14_4 => &generated::ITEMS_498,
        crate::PROTOCOL_1_15_2 => &generated::ITEMS_578,
        crate::PROTOCOL_1_16_5 => &generated::ITEMS_754,
        _ => return None,
    })
}

/// Resolves a historical block registry id to its namespaced key.
pub(crate) fn block_name(protocol: i32, id: i32) -> Option<&'static str> {
    let id = usize::try_from(id).ok()?;
    match protocol {
        crate::PROTOCOL_1_14_4 => generated::resolve(&generated::BLOCKS_498, id),
        crate::PROTOCOL_1_15_2 => generated::resolve(&generated::BLOCKS_578, id),
        crate::PROTOCOL_1_16_5 => generated::resolve(&generated::BLOCKS_754, id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_tables_keep_protocol_lengths_endpoints_and_unsupported_handling() {
        let cases = [
            (
                crate::PROTOCOL_1_14_4,
                877,
                "minecraft:air",
                "minecraft:campfire",
            ),
            (
                crate::PROTOCOL_1_15_2,
                884,
                "minecraft:air",
                "minecraft:honeycomb_block",
            ),
            (
                crate::PROTOCOL_1_16_5,
                976,
                "minecraft:air",
                "minecraft:respawn_anchor",
            ),
        ];

        for (protocol, expected_len, first, last) in cases {
            let table = item_table(protocol).expect("supported protocol has an item table");
            assert_eq!(table.len(), expected_len);
            assert_eq!(generated::resolve(table, 0), Some(first));
            assert_eq!(generated::resolve(table, table.len() - 1), Some(last));
        }

        assert!(item_table(0).is_none());
        assert!(item_id(0, "minecraft:air").is_none());
    }
}
