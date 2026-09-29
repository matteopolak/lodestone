//! Fixed witnesses read separately from the official 26.2 and 26.3 reports.

use lodestone_data::{block_states::StateId, item::Item};
use lodestone_v26_3::id_translation::{
    WireBlockStateId, WireItemId, block_state_from_wire, block_state_to_wire, item_from_wire,
    item_to_wire,
};

#[test]
fn report_witnesses_cover_unchanged_and_shifted_ids() {
    for line in include_str!("fixtures/id_translation_samples_26_3.tsv").lines() {
        let mut fields = line.split('\t');
        let kind = fields.next().unwrap();
        let name = fields.next().unwrap();
        let canonical = fields.next().unwrap().parse::<u32>().unwrap();
        let wire = fields.next().unwrap().parse::<u32>().unwrap();
        assert!(fields.next().is_none());
        match kind {
            "block_state" => {
                let state = StateId::from_state_str(name).unwrap();
                assert_eq!(state.raw(), canonical, "{name}");
                let wire_id = WireBlockStateId::new(wire).unwrap();
                assert_eq!(block_state_to_wire(state), wire_id, "{name}");
                assert_eq!(block_state_from_wire(wire_id), Ok(state), "{name}");
            }
            "item" => {
                let item = Item::from_name(name).unwrap();
                assert_eq!(u32::from(item.registry_id()), canonical, "{name}");
                let wire_id = WireItemId::new(wire).unwrap();
                assert_eq!(item_to_wire(item), wire_id, "{name}");
                assert_eq!(item_from_wire(wire_id), Ok(item), "{name}");
            }
            _ => panic!("unknown fixture kind {kind}"),
        }
    }
}

#[test]
fn all_canonical_ids_round_trip_and_new_wire_ids_fail() {
    for raw in 0..lodestone_data::block_states::STATE_COUNT {
        let state = StateId::new(raw).unwrap();
        assert_eq!(block_state_from_wire(block_state_to_wire(state)), Ok(state));
    }
    for raw in 0..u32::from(Item::COUNT) {
        let item = Item::from_registry_id(raw as u16).unwrap();
        assert_eq!(item_from_wire(item_to_wire(item)), Ok(item));
    }

    let unsupported_states = (0..WireBlockStateId::COUNT)
        .filter(|&raw| block_state_from_wire(WireBlockStateId::new(raw).unwrap()).is_err())
        .count();
    let unsupported_items = (0..WireItemId::COUNT)
        .filter(|&raw| item_from_wire(WireItemId::new(raw).unwrap()).is_err())
        .count();
    assert_eq!(unsupported_states, 35723 - 32366);
    assert_eq!(unsupported_items, 1658 - 1537);
}

#[test]
fn new_only_ids_are_rejected_without_aliasing_a_canonical_entry() {
    // The 26.3 reports assign these IDs to poplar planks.
    let poplar_state = WireBlockStateId::new(27).unwrap();
    let poplar_item = WireItemId::new(72).unwrap();
    assert_eq!(
        block_state_from_wire(poplar_state),
        Err(lodestone_v26_3::id_translation::UnsupportedWireBlockState(
            poplar_state
        ))
    );
    assert_eq!(
        item_from_wire(poplar_item),
        Err(lodestone_v26_3::id_translation::UnsupportedWireItem(
            poplar_item
        ))
    );
    assert!(WireBlockStateId::new(WireBlockStateId::COUNT).is_none());
    assert!(WireItemId::new(WireItemId::COUNT).is_none());
}
