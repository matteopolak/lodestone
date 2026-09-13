//! Consumer controls for protocol-498/578/754 bulk block changes and events.
//!
//! Packet bodies are literal wire bytes, not output from this crate's packet
//! encoders. The bulk fixtures exercise both historical chunk-record and
//! section-record layouts, then query the real world sink to prove that each
//! protocol-local state id was translated before storage.

use lodestone_model::{
    route, BlockPos, ClientEvent, ConnectionState, Directive, SectionPos, VersionAdapter,
};
use lodestone_v1_14::{
    packet_ids, packet_ids_498, packet_ids_578, adapter_for, PROTOCOL_1_14_4,
    PROTOCOL_1_15_2, PROTOCOL_1_16_5,
};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

fn loaded_world(pos: ChunkPos) -> World {
    let mut world = World::new();
    let column = ChunkColumn::new(
        0,
        16,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        lodestone_data::block_states::air_state_id(),
        0,
    );
    world.load(
        pos,
        LoadedChunk::new(column, ColumnLight::new(16), Heightmaps::new(), Vec::new()),
    );
    world
}

fn clientbound_id(protocol: i32, name: &str) -> i32 {
    let entries = match protocol {
        PROTOCOL_1_14_4 => packet_ids_498::play::clientbound::ENTRIES,
        PROTOCOL_1_15_2 => packet_ids_578::play::clientbound::ENTRIES,
        PROTOCOL_1_16_5 => packet_ids::play::clientbound::ENTRIES,
        _ => unreachable!("test protocols only"),
    };
    entries
        .iter()
        .find_map(|(entry, id)| (*entry == name).then_some(*id))
        .expect("generated table contains the fixture packet")
}

#[test]
fn literal_legacy_multi_block_change_updates_the_canonical_world() {
    // chunk=(-2,3), one record at local (10,70,4).  The source ids are
    // 3353 in both 1.14.4 and 1.15.2, where the historical reports name it
    // diamond_block. The expected state comes from the canonical registry,
    // independently of either adapter's table.
    let body = [
        0xff, 0xff, 0xff, 0xfe, // chunk x = -2
        0x00, 0x00, 0x00, 0x03, // chunk z = 3
        0x01, // record count
        0xa4, 0x46, 0x99, 0x1a, // x=10, z=4, y=70, state=3353
    ];
    let expected = lodestone_data::block_states::state_id("minecraft:diamond_block")
        .expect("canonical registry contains diamond_block");

    for protocol in [PROTOCOL_1_14_4, PROTOCOL_1_15_2] {
        let mut world = loaded_world(ChunkPos::new(-2, 3));
        let directives = adapter_for(protocol)
            .handle_packet(
                &mut world,
                ConnectionState::Play,
                clientbound_id(protocol, "minecraft:multi_block_change"),
                &body,
            )
            .expect("literal legacy multi_block_change body decodes");
        assert_eq!(world.block_state_at(-22, 70, 52), Some(expected));
        assert_eq!(
            directives,
            vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
                section: SectionPos::new(-2, 4, 3),
                blocks: vec![[10, 6, 4]],
            })],
            "protocol {protocol} must notify the section mesh consumer",
        );
    }
}

#[test]
fn literal_754_section_multi_block_change_updates_the_canonical_world() {
    // Section (-2,4,3), local (10,6,4), source state 3355 = diamond_block.
    // Header layout is x(22) | z(22) | section_y(20), and the record layout
    // is state << 12 | local_x << 8 | local_z << 4 | local_y.
    let body = [
        0xff, 0xff, 0xf8, 0x00, 0x00, 0x30, 0x00, 0x04, // section (-2,4,3)
        0x00, // trust edges = false
        0x01, // record count
        0xc6, 0xf4, 0xc6, 0x06, // (state=3355 << 12) | local=10,6,4
    ];
    let expected = lodestone_data::block_states::state_id("minecraft:diamond_block")
        .expect("canonical registry contains diamond_block");
    let mut world = loaded_world(ChunkPos::new(-2, 3));
    let directives = adapter_for(PROTOCOL_1_16_5)
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            clientbound_id(PROTOCOL_1_16_5, "minecraft:multi_block_change"),
            &body,
        )
        .expect("literal section multi_block_change body decodes");

    assert_eq!(world.block_state_at(-22, 70, 52), Some(expected));
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: SectionPos::new(-2, 4, 3),
            blocks: vec![[10, 6, 4]],
        })]
    );
}

#[test]
fn literal_block_action_reaches_the_visible_block_event_route_for_each_protocol() {
    // Packed 1.14+ position (-12,63,34), parameters 1 and 9. The block type
    // ids are registration ids: chest is 145 in 498/578 and 147 in 754.
    let positions_and_prefixes = [
        (PROTOCOL_1_14_4, 145_i32),
        (PROTOCOL_1_15_2, 145_i32),
        (PROTOCOL_1_16_5, 147_i32),
    ];
    for (protocol, block_id) in positions_and_prefixes {
        let body = match block_id {
            145 => [
                0xff, 0xff, 0xfd, 0x00, 0x00, 0x02, 0x20, 0x3f, 1, 9, 0x91, 0x01,
            ],
            147 => [
                0xff, 0xff, 0xfd, 0x00, 0x00, 0x02, 0x20, 0x3f, 1, 9, 0x93, 0x01,
            ],
            _ => unreachable!(),
        };
        let directives = adapter_for(protocol)
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                clientbound_id(protocol, "minecraft:block_action"),
                &body,
            )
            .expect("literal block_action body decodes");
        let [Directive::Emit(event)] = directives.as_slice() else {
            panic!("expected one block event for {protocol}, got {directives:?}");
        };
        let ClientEvent::BlockEvent { pos, b0, b1, block } = event else {
            panic!("expected BlockEvent for {protocol}, got {event:?}");
        };
        assert_eq!(*pos, BlockPos::new(-12, 63, 34));
        assert_eq!((*b0, *b1), (1, 9));
        assert_eq!(block.to_string(), "minecraft:chest");
        assert!(route(event).must_forward());
    }
}
