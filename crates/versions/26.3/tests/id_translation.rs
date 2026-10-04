//! Fixed witnesses read separately from the official 26.2 and 26.3 reports.

use lodestone_data::{GameDataVersion, block_states::StateId, item::Item};
use lodestone_v26_3::id_translation::{
    WireBlockStateId, WireItemId, block_state_from_wire, block_state_to_wire, item_from_wire,
    item_to_wire,
};

#[test]
fn report_witnesses_cover_unchanged_shifted_and_new_ids() {
    let mut witnessed = 0;
    for line in include_str!("fixtures/id_translation_samples_26_3.tsv").lines() {
        let mut fields = line.split('\t');
        let kind = fields.next().unwrap();
        let name = fields.next().unwrap();
        let canonical = fields.next().unwrap().parse::<u32>().unwrap();
        let wire = fields.next().unwrap().parse::<u32>().unwrap();
        assert!(fields.next().is_none());
        witnessed += 1;
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
    assert_eq!(witnessed, 17);
}

#[test]
fn every_wire_entry_reaches_a_distinct_canonical_identity() {
    assert_eq!(WireBlockStateId::COUNT, 35_723);
    assert_eq!(WireItemId::COUNT, 1_658);
    let mut states = vec![false; lodestone_data::block_states::STATE_COUNT as usize];
    for raw in 0..WireBlockStateId::COUNT {
        let state = block_state_from_wire(WireBlockStateId::new(raw).unwrap()).unwrap();
        assert!(!std::mem::replace(&mut states[state.index()], true), "state {raw}");
        assert_eq!(block_state_to_wire(state).raw(), raw);
    }
    assert!(states.into_iter().all(|present| present));
    let mut items = vec![false; usize::from(Item::COUNT)];
    for raw in 0..WireItemId::COUNT {
        let item = item_from_wire(WireItemId::new(raw).unwrap()).unwrap();
        assert!(!std::mem::replace(&mut items[usize::from(item.registry_id())], true), "item {raw}");
        assert_eq!(item_to_wire(item).raw(), raw);
    }
    assert!(items.into_iter().all(|present| present));
}

#[test]
fn new_only_ids_reach_the_union_and_refuse_old_release_egress() {
    let poplar_state = WireBlockStateId::new(27).unwrap();
    let poplar_item = WireItemId::new(72).unwrap();
    let state = block_state_from_wire(poplar_state).unwrap();
    let item = item_from_wire(poplar_item).unwrap();
    assert_eq!(state.raw(), 32_366);
    assert_eq!(state, StateId::from_state_str("minecraft:poplar_planks").unwrap());
    assert_ne!(state, StateId::new(27).unwrap());
    assert_eq!(item.registry_id(), 1_537);
    assert_eq!(item, Item::from_name("minecraft:poplar_planks").unwrap());
    assert_ne!(item, Item::from_registry_id(72).unwrap());
    assert_eq!(GameDataVersion::V26_2.state_to_wire(state), None);
    assert_eq!(GameDataVersion::V26_2.item_to_wire(item), None);
    let high = StateId::new(35_722).unwrap();
    assert_eq!(block_state_to_wire(high).raw(), 18_402);
    assert_eq!(GameDataVersion::V26_2.state_to_wire(high), None);
    let map = Item::from_name("minecraft:warm_ocean_ruins_map").unwrap();
    assert_eq!(map.registry_id(), 1_657);
    assert_eq!(item_to_wire(map).raw(), 1_254);
    assert_eq!(GameDataVersion::V26_2.item_to_wire(map), None);
}

#[test]
fn wire_ranges_reject_overflow_and_preserve_high_valid_values() {
    assert_eq!(WireBlockStateId::new(35_722).unwrap().raw(), 35_722);
    assert_eq!(WireItemId::new(1_657).unwrap().raw(), 1_657);
    assert!(WireBlockStateId::new(WireBlockStateId::COUNT).is_none());
    assert!(WireItemId::new(WireItemId::COUNT).is_none());
    assert!(WireBlockStateId::new(u32::MAX).is_none());
    assert!(WireItemId::new(u32::MAX).is_none());
}
