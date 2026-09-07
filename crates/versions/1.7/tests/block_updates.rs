//! Literal protocol-5 block mutation and block-event consumer controls.
//!
//! The bodies are assembled from the wire field widths rather than this
//! crate's packet encoders.  Assertions query the real `World` sink and the
//! shared event route so a successful decode cannot hide a dropped write or
//! event.

use lodestone_model::{
    route, BlockPos, ClientEvent, ConnectionState, Directive, SectionPos, VersionAdapter,
};
use lodestone_v1_7::{packet_ids::play, V5Adapter};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

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

#[test]
fn literal_block_change_updates_the_canonical_world_and_dirty_signal() {
    // Position (11,64,-5), followed by protocol-5 block id 56 (diamond ore)
    // and metadata 0.  The position is written as i32, u8, i32 and the block
    // id as a VarInt, exactly as the packet body carries it.
    let body = [
        0x00, 0x00, 0x00, 0x0b, 0x40, 0xff, 0xff, 0xff, 0xfb, 0x38, 0x00,
    ];
    let expected = lodestone_data::block_states::state_id("minecraft:diamond_ore")
        .expect("canonical registry contains diamond_ore");
    let mut world = loaded_world(ChunkPos::new(0, -1));
    let directives = V5Adapter::new()
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::BLOCK_CHANGE,
            &body,
        )
        .expect("literal block_change body decodes");

    assert_eq!(world.block_state_at(11, 64, -5), Some(expected));
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: SectionPos::new(0, 4, -1),
            blocks: vec![[11, 0, 11]],
        })]
    );
}

#[test]
fn literal_multi_block_change_updates_canonical_world_in_wire_order() {
    // Chunk (-2,3), count 2, declared body length 8.  Records arrive at
    // (10,70,4) and (1,85,12), both protocol block id 56 / metadata 0.
    let body = [
        0xff, 0xff, 0xff, 0xfe, 0x00, 0x00, 0x00, 0x03, // chunk (-2,3)
        0x00, 0x02, // record count
        0x00, 0x00, 0x00, 0x08, // declared record bytes
        0x03, 0x80, 0x46, 0x4a, // (x=10,y=70,z=4), state=(56 << 4)
        0x03, 0x80, 0x55, 0xc1, // (x=1,y=85,z=12), state=(56 << 4)
    ];
    let expected = lodestone_data::block_states::state_id("minecraft:diamond_ore")
        .expect("canonical registry contains diamond_ore");
    let mut world = loaded_world(ChunkPos::new(-2, 3));
    let directives = V5Adapter::new()
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::MULTI_BLOCK_CHANGE,
            &body,
        )
        .expect("literal multi_block_change body decodes");

    assert_eq!(world.block_state_at(-22, 70, 52), Some(expected));
    assert_eq!(world.block_state_at(-31, 85, 60), Some(expected));
    assert_eq!(
        directives,
        vec![
            Directive::Emit(ClientEvent::SectionBlocksChanged {
                section: SectionPos::new(-2, 4, 3),
                blocks: vec![[10, 6, 4]],
            }),
            Directive::Emit(ClientEvent::SectionBlocksChanged {
                section: SectionPos::new(-2, 5, 3),
                blocks: vec![[1, 5, 12]],
            }),
        ]
    );
}

#[test]
fn literal_block_action_reaches_the_visible_event_route() {
    // Position (1,64,-3), parameters 1 and 2, protocol block type 54
    // (chest).  The position uses the packet's i32/i16/i32 widths.
    let body = [
        0x00, 0x00, 0x00, 0x01, 0x00, 0x40, 0xff, 0xff, 0xff, 0xfd, 0x01, 0x02, 0x36,
    ];
    let directives = V5Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::BLOCK_ACTION,
            &body,
        )
        .expect("literal block_action body decodes");
    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one block event, got {directives:?}");
    };
    let ClientEvent::BlockEvent { pos, b0, b1, block } = event else {
        panic!("expected BlockEvent, got {event:?}");
    };
    assert_eq!(*pos, BlockPos::new(1, 64, -3));
    assert_eq!((*b0, *b1), (1, 2));
    assert_eq!(block.to_string(), "minecraft:chest");
    assert!(route(event).must_forward());
}
