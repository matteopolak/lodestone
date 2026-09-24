//! Consumer controls for protocol-404 bulk block changes and block events.
//!
//! The packet bodies are literal bytes assembled from the protocol field
//! widths. They deliberately use negative chunk coordinates, section-crossing
//! Y values, and a state whose protocol id differs from its canonical id. The
//! assertions run through the real [`lodestone_world::World`] sink so a test
//! cannot pass when the adapter merely emits a dirty signal and drops the
//! authoritative block write.

use lodestone_model::{
    route, BlockPos, ClientEvent, ConnectionState, Directive, SectionPos, VersionAdapter,
};
use lodestone_v1_13::{packet_ids::play, V404Adapter};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

fn loaded_world() -> World {
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
        ChunkPos::new(-2, 3),
        LoadedChunk::new(column, ColumnLight::new(16), Heightmaps::new(), Vec::new()),
    );
    world
}

#[test]
fn literal_multi_block_change_updates_canonical_world_and_each_dirty_section() {
    // chunk=(-2,3), count=2:
    //   local (10,70,4), wire state 3050 = diamond_block
    //   local (1,85,12), wire state 1 = stone
    // The two records land in different sections.  The state ids and VarInts
    // are taken from the committed 1.13.2 report; this body is not produced by
    // the packet encoder under test.
    let body = [
        0xff, 0xff, 0xff, 0xfe, // chunk x = -2
        0x00, 0x00, 0x00, 0x03, // chunk z = 3
        0x02, // record count
        0xa4, 0x46, 0xea, 0x17, // x=10, z=4, y=70, state=3050
        0x1c, 0x55, 0x01, // x=1, z=12, y=85, state=1
    ];

    let mut world = loaded_world();
    let directives = V404Adapter::new()
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::MULTI_BLOCK_CHANGE,
            &body,
        )
        .expect("literal multi_block_change body decodes");

    let diamond = lodestone_data::block_states::state_id("minecraft:diamond_block")
        .expect("canonical registry contains diamond_block");
    let stone = lodestone_data::block_states::state_id("minecraft:stone")
        .expect("canonical registry contains stone");
    assert_eq!(world.block_state_at(-22, 70, 52), Some(diamond));
    assert_eq!(world.block_state_at(-31, 85, 60), Some(stone));

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
        ],
        "bulk writes must notify the section mesh consumer with the same local cells",
    );
}

#[test]
fn literal_block_action_reaches_the_visible_block_event_route() {
    // Packed pre-1.14 position (1,64,-3), b0=1, b1=2, block type 142
    // (chest).  These bytes use the wire's x/y/z packing directly.
    let body = [
        0x00, 0x00, 0x00, 0x41, 0x03, 0xff, 0xff, 0xfd, // position
        0x01, 0x02, // event parameters
        0x8e, 0x01, // block type 142
    ];
    let event = V404Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::BLOCK_ACTION,
            &body,
        )
        .expect("literal block_action body decodes");
    let [Directive::Emit(event)] = event.as_slice() else {
        panic!("expected one block event, got {event:?}");
    };
    let ClientEvent::BlockEvent { pos, b0, b1, block } = event else {
        panic!("expected BlockEvent, got {event:?}");
    };
    assert_eq!(*pos, BlockPos::new(1, 64, -3));
    assert_eq!((*b0, *b1), (1, 2));
    assert_eq!(block.to_string(), "minecraft:chest");
    assert!(route(event).must_forward());
}
