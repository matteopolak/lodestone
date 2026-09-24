//! Literal protocol-774 block mutation and block-event fixtures.
//!
//! The mutation fixtures assert both sides of the adapter seam: bulk updates
//! are handed to `WorldSink::set_blocks` as one canonicalized section batch,
//! while block events reach the shared event model with a canonical block key.

use lodestone_core::Nbt;
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter, WorldSink};
use lodestone_v1_21_11::{packet_ids::play, V774Adapter};
use lodestone_world::{BiomePatch, BlockEntitySync, ChunkPos, ColumnPatch, LightPatch, LoadedChunk};

#[derive(Default)]
struct RecordingSink {
    single: Vec<(i32, i32, i32, u32)>,
    batches: Vec<(i32, i32, i32, Vec<(u8, u8, u8, u32)>)>,
    syncs: Vec<(i32, i32, i32, Option<u32>)>,
}

impl WorldSink for RecordingSink {
    fn load(&mut self, _pos: ChunkPos, _chunk: LoadedChunk) {}
    fn merge(&mut self, _pos: ChunkPos, _patch: ColumnPatch) {}
    fn set_block(&mut self, x: i32, y: i32, z: i32, state: u32) {
        self.single.push((x, y, z, state));
    }
    fn set_blocks(
        &mut self,
        section_x: i32,
        section_y: i32,
        section_z: i32,
        blocks: &[(u8, u8, u8, u32)],
    ) {
        self.batches
            .push((section_x, section_y, section_z, blocks.to_vec()));
    }
    fn set_block_entity(&mut self, _x: i32, _y: i32, _z: i32, _type_id: u32, _nbt: Nbt) {}
    fn sync_block_entity(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        block_entity_type: Option<u32>,
    ) -> BlockEntitySync {
        self.syncs.push((x, y, z, block_entity_type));
        BlockEntitySync::ChunkAbsent
    }
    fn merge_light(&mut self, _pos: ChunkPos, _patch: LightPatch) {}
    fn merge_biomes(&mut self, _pos: ChunkPos, _patch: BiomePatch) {}
    fn unload(&mut self, _pos: ChunkPos) {}
}

fn dispatch(
    sink: &mut RecordingSink,
    id: i32,
    payload: &[u8],
) -> Vec<Directive> {
    V774Adapter::default()
        .handle_packet(sink, ConnectionState::Play, id, payload)
        .expect("literal protocol-774 payload should decode")
}

#[test]
fn section_blocks_update_batches_and_canonicalizes_literal_bytes() {
    // Section (1, -2, 3), count 2. The first record carries wire state 8720,
    // the property-free resin block in the pinned 1.21.11 state census. Its
    // canonical 26.2 state id is intentionally different from 8720.
    let payload = [
        0x00, 0x00, 0x04, 0x00, 0x00, 0x3f, 0xff, 0xfe, // packed section
        0x02, // two records
        0xb2, 0x82, 0x84, 0x11, // state 8720, local x=1,z=3,y=2
        0xf0, 0x3f, // state 1, local x=15,z=15,y=0
    ];
    let mut sink = RecordingSink::default();
    let directives = dispatch(&mut sink, play::clientbound::SECTION_BLOCKS_UPDATE, &payload);

    assert!(sink.single.is_empty(), "bulk packets must not degrade to single writes");
    assert_eq!(sink.batches.len(), 1);
    let (x, y, z, blocks) = &sink.batches[0];
    assert_eq!((*x, *y, *z), (1, -2, 3));
    assert_eq!(blocks.len(), 2);
    assert_eq!((blocks[0].0, blocks[0].1, blocks[0].2), (1, 2, 3));
    assert_eq!((blocks[1].0, blocks[1].1, blocks[1].2), (15, 0, 15));
    assert_eq!(lodestone_data::block_states::block_name(blocks[0].3), Some("minecraft:resin_block"));
    assert_ne!(blocks[0].3, 8720, "wire state ids must be canonicalized");
    assert_eq!(lodestone_data::block_states::block_name(blocks[1].3), Some("minecraft:stone"));

    assert_eq!(
        sink.syncs,
        vec![(17, -30, 51, None), (31, -32, 63, None)],
        "every canonicalized write must reach block-entity synchronization"
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: lodestone_model::SectionPos::new(1, -2, 3),
            blocks: vec![[1, 2, 3], [15, 0, 15]],
        })]
    );
}

#[test]
fn block_update_canonicalizes_literal_wire_state_before_world_write() {
    // Position (3, 70, -5), followed by wire state 8720.
    let payload = [0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xb0, 0x46, 0x90, 0x44];
    let mut sink = RecordingSink::default();
    let directives = dispatch(&mut sink, play::clientbound::BLOCK_UPDATE, &payload);

    assert_eq!(sink.single.len(), 1);
    assert_eq!((sink.single[0].0, sink.single[0].1, sink.single[0].2), (3, 70, -5));
    assert_eq!(lodestone_data::block_states::block_name(sink.single[0].3), Some("minecraft:resin_block"));
    assert_ne!(sink.single[0].3, 8720);
    assert_eq!(sink.batches, Vec::new());
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: lodestone_model::SectionPos::new(0, 4, -1),
            blocks: vec![[3, 6, 11]],
        })]
    );
}

#[test]
fn block_event_emits_canonical_block_and_preserves_literal_parameters() {
    // Position (3, 70, -5), parameters 7 and 2, block registry id 109
    // (note_block in the pinned protocol registry).
    let payload = [
        0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xb0, 0x46, 7, 2, 109,
    ];
    let mut sink = RecordingSink::default();
    let events = dispatch(&mut sink, play::clientbound::BLOCK_EVENT, &payload);
    assert_eq!(
        events,
        vec![Directive::Emit(ClientEvent::BlockEvent {
            pos: lodestone_model::BlockPos::new(3, 70, -5),
            b0: 7,
            b1: 2,
            block: "minecraft:note_block".parse().unwrap(),
        })]
    );
}
