//! What protocol 777 puts on the wire for block ids and for a shulker box's
//! `minecraft:container`.
//!
//! Every expected number is read off the official reports (`blocks.json`,
//! `registries.json`) of 26.2 (the canonical ids the server works in) and 26.3
//! (what the client expects), not from the translation tables under test.

use lodestone_core::Reader;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ClientEvent, ConnectionState, Directive, ItemStack, VersionAdapter};
use lodestone_server::{ServerDirective, ServerProtocol};
use lodestone_v26_3::packet_ids;
use lodestone_world::World;

fn send(directive: ServerDirective) -> Vec<u8> {
    let ServerDirective::Send { payload, .. } = directive else { panic!("expected one packet, got {directive:?}") };
    payload
}

fn stack(item: &str, count: u32) -> ItemStack {
    ItemStack::new(item.parse().unwrap(), count)
}

/// The default chest state: 3988 in the 26.2 report, 5544 in the 26.3 one.
fn chest() -> StateId {
    let state = StateId::from_state_str("minecraft:chest[facing=north,type=single,waterlogged=false]").unwrap();
    assert_eq!(state.raw(), 3988, "26.2 blocks.json default state of the chest");
    state
}

#[test]
fn a_block_update_carries_the_26_3_state_id() {
    let pos = BlockPos::new(1, 2, 3);
    let payload = send(lodestone_v26_3::server_protocol().encode_block_update(pos.x, pos.y, pos.z, chest()));
    let mut reader = Reader::new(&payload);
    reader.i64().unwrap();
    assert_eq!(reader.var_i32().unwrap(), 5544, "26.3 blocks.json default state of the chest");
    reader.ensure_empty().unwrap();

    // A state added by 26.3 sits past the end of 26.2's range.
    let bush = StateId::from_state_str("minecraft:firefly_bush").unwrap();
    assert_eq!(bush.raw(), 32365);
    let payload = send(lodestone_v26_3::server_protocol().encode_block_update(0, 0, 0, bush));
    assert_eq!(Reader::new(&payload[8..]).var_i32().unwrap(), 35722);
}

#[test]
fn a_block_event_carries_the_26_3_block_registry_id() {
    let pos = BlockPos::new(1, 2, 3);
    for (block, id) in [("minecraft:chest", 245), ("minecraft:jukebox", 332), ("minecraft:note_block", 118)] {
        let payload = send(lodestone_v26_3::server_protocol().encode_block_event(pos, block, 1, 2));
        let mut reader = Reader::new(&payload);
        reader.i64().unwrap();
        assert_eq!((reader.u8().unwrap(), reader.u8().unwrap()), (1, 2));
        assert_eq!(reader.var_i32().unwrap(), id, "{block} in the 26.3 minecraft:block registry");
        reader.ensure_empty().unwrap();
    }
}

#[test]
fn a_destroy_particle_event_carries_the_26_3_state_id() {
    let payload = send(lodestone_v26_3::server_protocol().encode_level_event(
        lodestone_server::effects::PARTICLES_DESTROY_BLOCK,
        BlockPos::new(0, 0, 0),
        chest().raw() as i32,
        false,
    ));
    let mut reader = Reader::new(&payload);
    assert_eq!(reader.i32().unwrap(), 2001);
    reader.i64().unwrap();
    assert_eq!(reader.i32().unwrap(), 5544);
}

/// A shulker box holding five stone, nothing, then two cobblestone.
fn loaded_box() -> ItemStack {
    let mut shulker = stack("minecraft:shulker_box", 1);
    shulker.components.container = vec![Some(stack("minecraft:stone", 5)), None, Some(stack("minecraft:cobblestone", 2))];
    shulker
}

/// The slot-update body for [`loaded_box`], derived by hand from the
/// container codec (a list of optional item-first templates) and the 26.3
/// reports: shulker_box is item 656 (`90 05`), stone 1, cobblestone 62, and
/// `minecraft:container` is data component 77 (`4d`).
const LOADED_BOX: [u8; 22] = [
    0, 0, 0, 36, // window 0, state 0, slot 36
    1, 0x90, 0x05, // count, item
    1, 0, 0x4d, // one added component, none removed, container
    3, // three slots
    1, 1, 5, 0, 0, // stone x5, empty patch
    0, // empty slot
    1, 62, 2, 0, 0, // cobblestone x2, empty patch
];

#[test]
fn the_container_component_is_written_as_the_codec_lays_it_out() {
    let payload = send(lodestone_v26_3::server_protocol().encode_container_slot(0, 0, 36, Some(&loaded_box())));
    assert_eq!(payload, LOADED_BOX);
}

#[test]
fn an_empty_box_writes_no_container_component() {
    let payload = send(lodestone_v26_3::server_protocol().encode_container_slot(0, 0, 36, Some(&stack("minecraft:shulker_box", 1))));
    assert_eq!(payload, [0, 0, 0, 36, 1, 0x90, 0x05, 0, 0]);
}

#[test]
fn the_container_component_reaches_the_client_decoded() {
    let directives = lodestone_v26_3::adapter()
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_ids::play::clientbound::CONTAINER_SET_SLOT, &LOADED_BOX)
        .expect("a 26.3 container_set_slot decodes");
    let [Directive::Emit(ClientEvent::ContainerSlot { item: Some(item), .. })] = directives.as_slice() else {
        panic!("expected one ContainerSlot with an item, got {directives:?}");
    };
    assert_eq!(item.item.to_string(), "minecraft:shulker_box");
    // The decoder fills in prototype defaults, so compare identity and count.
    let held: Vec<Option<(String, u32)>> =
        item.components.container.iter().map(|slot| slot.as_ref().map(|s| (s.item.to_string(), s.count))).collect();
    assert_eq!(
        held,
        [Some(("minecraft:stone".to_owned(), 5)), None, Some(("minecraft:cobblestone".to_owned(), 2))]
    );
}

/// A decorated pot with sherds on the back and front only. In 26.3
/// `minecraft:pot_decorations` (component 76) is four optional item templates
/// in back, left, right, front order, a plain face being absent. Item ids from
/// the 26.3 report: decorated_pot 392 (`88 03`), angler sherd 1598 (`be 0c`),
/// archer sherd 1599 (`bf 0c`).
const SHERDED_POT: [u8; 18] = [
    0, 0, 0, 36, // window 0, state 0, slot 36
    1, 0x88, 0x03, // count, item
    1, 0, 76, // one added component, none removed, pot_decorations
    1, 0xbe, 0x0c, 1, 0, 0, // back: angler sherd x1, empty patch
    0, 0, // left, right absent
];

#[test]
fn pot_decorations_are_four_optional_templates_in_26_3() {
    let mut pot = stack("minecraft:decorated_pot", 1);
    pot.components.pot_decorations = Some(lodestone_model::PotDecorations {
        back: Some("minecraft:angler_pottery_sherd".parse().unwrap()),
        front: Some("minecraft:archer_pottery_sherd".parse().unwrap()),
        ..Default::default()
    });
    let payload = send(lodestone_v26_3::server_protocol().encode_container_slot(0, 0, 36, Some(&pot)));
    let mut expected = SHERDED_POT.to_vec();
    expected.extend_from_slice(&[1, 0xbf, 0x0c, 1, 0, 0]);
    assert_eq!(payload, expected);
    let directives = lodestone_v26_3::adapter()
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_ids::play::clientbound::CONTAINER_SET_SLOT, &payload)
        .expect("decodes");
    let [Directive::Emit(ClientEvent::ContainerSlot { item: Some(item), .. })] = directives.as_slice() else {
        panic!("expected one ContainerSlot, got {directives:?}");
    };
    assert_eq!(item.components.pot_decorations, pot.components.pot_decorations);
}
