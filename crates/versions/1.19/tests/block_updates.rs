//! Production-consumer controls for protocol-762 block updates.
//!
//! These packet bodies are literal wire bytes assembled independently of the
//! implementation's encoders. The bulk fixture proves canonical state
//! translation reaches the loaded world and the event fixture proves the
//! block-event route retains its visible parameters.

use lodestone_model::{route, BlockPos, ClientEvent, ConnectionState, Directive, SectionPos, VersionAdapter};
use lodestone_v1_19::{packet_ids, adapter_for, PROTOCOL_1_19_4};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

#[test]
fn literal_multi_block_change_updates_canonical_world_and_section_signal() {
    // Section (-2,-4,3), no light suppression, one record: state=4272
    // (diamond_block in the protocol-local palette), local (10,6,4).
    let body = [
        0xff, 0xff, 0xf8, 0x00, 0x00, 0x3f, 0xff, 0xfc, 0x00, 0x01, 0xc6, 0x94, 0xac, 0x08,
    ];
    let expected = lodestone_data::block_states::state_id("minecraft:diamond_block")
        .expect("canonical registry contains diamond_block");
    let mut world = World::new();
    world.load(
        ChunkPos::new(-2, 3),
        LoadedChunk::new(
            ChunkColumn::new(
                -64,
                24,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                lodestone_data::block_states::air_state_id(),
                0,
            ),
            ColumnLight::new(24),
            Heightmaps::new(),
            Vec::new(),
        ),
    );

    let directives = adapter_for(PROTOCOL_1_19_4)
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            packet_ids::play::clientbound::MULTI_BLOCK_CHANGE,
            &body,
        )
        .expect("literal multi_block_change body decodes");

    assert_eq!(world.block_state_at(-22, -58, 52), Some(expected));
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: SectionPos::new(-2, -4, 3),
            blocks: vec![[10, 6, 4]],
        })]
    );
}

#[test]
fn literal_block_action_reaches_the_visible_block_event_route() {
    // Packed position (-12,63,34), note-block parameters (0,6), and the
    // protocol-762 note-block registry id 101.
    let body = [
        0xff, 0xff, 0xfd, 0x00, 0x00, 0x02, 0x20, 0x3f, 0, 6, 0x65,
    ];
    let directives = adapter_for(PROTOCOL_1_19_4)
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_ids::play::clientbound::BLOCK_ACTION,
            &body,
        )
        .expect("literal block_action body decodes");
    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one block event, got {directives:?}");
    };
    let ClientEvent::BlockEvent { pos, b0, b1, block } = event else {
        panic!("expected BlockEvent, got {event:?}");
    };
    assert_eq!(*pos, BlockPos::new(-12, 63, 34));
    assert_eq!((*b0, *b1), (0, 6));
    assert_eq!(block.to_string(), "minecraft:note_block");
    assert!(route(event).must_forward());
}
