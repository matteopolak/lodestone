//! Production-consumer controls for protocol-756/758 block updates.
//!
//! The packet bodies below are literal wire bytes assembled from the protocol
//! field widths. They cover both section-Y interpretations and use a state id
//! that must pass through the era's canonical table before it reaches the
//! world store. The assertions then inspect the world and the visible-event
//! route, rather than only checking that a decoder returned a directive.

use lodestone_model::{route, BlockPos, ClientEvent, ConnectionState, Directive, SectionPos, VersionAdapter};
use lodestone_v1_17::{packet_ids, packet_ids_758, V756Adapter, PROTOCOL_1_17_1, PROTOCOL_1_18_2};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

fn loaded_world(chunk: ChunkPos, min_y: i32, height: i32) -> World {
    let mut world = World::new();
    let column = ChunkColumn::new(
        min_y,
        height as usize,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        lodestone_data::block_states::air_state_id(),
        0,
    );
    world.load(
        chunk,
        LoadedChunk::new(column, ColumnLight::new(height as usize / 16), Heightmaps::new(), Vec::new()),
    );
    world
}

#[test]
fn literal_multi_block_change_updates_canonical_world_for_both_section_y_forms() {
    // The state is diamond_block in each protocol's local table (3412), while
    // the expected value is obtained independently from the canonical table.
    // The same local cell is used in different vertical sections so the
    // unsigned 756 and signed 758 header interpretations are both exercised.
    let diamond = lodestone_data::block_states::state_id("minecraft:diamond_block")
        .expect("canonical registry contains diamond_block");
    let fixtures = [
        (
            PROTOCOL_1_17_1,
            packet_ids::play::clientbound::MULTI_BLOCK_CHANGE,
            // Section (-2, 4, 3), not-trust-edges=false, count=1,
            // record state=3412 and local (x=10,z=4,y=6).
            [
                0xff, 0xff, 0xf8, 0x00, 0x00, 0x30, 0x00, 0x04, 0x00, 0x01, 0xc6, 0x94,
                0xd5, 0x06,
            ],
            ChunkPos::new(-2, 3),
            0,
            16,
            -22,
            70,
            52,
            4,
        ),
        (
            PROTOCOL_1_18_2,
            packet_ids_758::play::clientbound::MULTI_BLOCK_CHANGE,
            // Section (-2, -4, 3), with the same record. At 758 the twenty
            // bit section-Y field is signed, so this lands at y=-58.
            [
                0xff, 0xff, 0xf8, 0x00, 0x00, 0x3f, 0xff, 0xfc, 0x00, 0x01, 0xc6, 0x94,
                0xd5, 0x06,
            ],
            ChunkPos::new(-2, 3),
            -64,
            24,
            -22,
            -58,
            52,
            -4,
        ),
    ];

    for (protocol, packet_id, body, chunk, min_y, height, x, y, z, section_y) in fixtures {
        let mut world = loaded_world(chunk, min_y, height);
        let directives = V756Adapter::for_protocol(protocol)
            .handle_packet(&mut world, ConnectionState::Play, packet_id, &body)
            .expect("literal multi_block_change body decodes");

        assert_eq!(world.block_state_at(x, y, z), Some(diamond));
        assert_eq!(
            directives,
            vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
                section: SectionPos::new(-2, section_y, 3),
                blocks: vec![[10, 6, 4]],
            })],
            "protocol {protocol} must notify the section mesh consumer",
        );
    }
}

#[test]
fn literal_block_action_reaches_the_visible_block_event_route_for_both_protocols() {
    // Packed position (-12,63,34), chest parameters (1,9), protocol-local
    // block registry id 153. The bytes are intentionally not produced by the
    // packet encoder under test.
    let body = [
        0xff, 0xff, 0xfd, 0x00, 0x00, 0x02, 0x20, 0x3f, 1, 9, 0x99, 0x01,
    ];
    for (protocol, packet_id) in [
        (PROTOCOL_1_17_1, packet_ids::play::clientbound::BLOCK_ACTION),
        (PROTOCOL_1_18_2, packet_ids_758::play::clientbound::BLOCK_ACTION),
    ] {
        let directives = V756Adapter::for_protocol(protocol)
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                packet_id,
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
