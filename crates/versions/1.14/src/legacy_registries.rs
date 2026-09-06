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
