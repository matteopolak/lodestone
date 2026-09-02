//! Hermetic tests for the protocol 47 (Minecraft 1.8.9) `block_change` and
//! `multi_block_change` packets.
//!
//! These packets mutate the client-owned world rather than emitting events, so
//! the assertions run against a recording [`WorldSink`] that captures the
//! exact `set_block` calls the adapter routes — the same pattern
//! `lodestone-v1-9`'s own `tests/block_updates.rs` uses for the identically-
//! shaped 1.12.2 packets, and `lodestone-v26-2`'s `tests/block_updates.rs` uses
//! for the modern `block_update`/`section_blocks_update` packets. Golden byte
//! vectors are hand-assembled from minecraft-data's 1.8 `protocol.json`, never
//! from this crate's own `Encode` derive, so a symmetric encode/decode bug
//! cannot pass silently. The wire shape is additionally corroborated by
//! `tests/live_interaction.rs`'s independently hand-rolled `decode_block_change`,
//! which decodes real bytes captured from a live 1.8.9 server.
//!
//! The expected canonical block-state ids come from
//! `lodestone_canonical::canonical::resolve_or_air` directly — that bridge is
//! independently verified elsewhere (against the real 1.13.2 server jar's own
//! flattening fix) and is used here purely as the already-established oracle
//! for "what should this `(id, meta)` pair resolve to", not as part of the
//! code path under test (single/multi block writes, not chunk-column decode).
//!
//! One field's bit order (`multi_block_change`'s `horizontalPos` nibble
//! packing) is **not** cross-checked against minecraft-data or a live capture
//! in this pass — `protocol.json` gives the field's width but not which
//! nibble is X and which is Z. It is taken from the long-stable external wire
//! documentation for this exact, decade-old packet shape and flagged here
//! rather than presented as jar-verified.

use lodestone_canonical::canonical::{self, FallbackTally};
use lodestone_core::Nbt;
use lodestone_model::{ConnectionState, SectionPos, VersionAdapter};
use lodestone_v1_8::V47Adapter;
use lodestone_v1_8::packet_ids::play;
use lodestone_v1_8::packets::position::pack_position;
use lodestone_world::{
    BiomePatch, BlockEntitySync, ChunkPos, ColumnPatch, LightPatch, LoadedChunk, WorldSink,
};

/// A [`WorldSink`] that records single-block writes for assertion.
#[derive(Default)]
struct RecordingSink {
    set_block: Vec<(i32, i32, i32, u32)>,
    sync_block_entity: Vec<(i32, i32, i32, Option<u32>)>,
}

impl WorldSink for RecordingSink {
    fn load(&mut self, _pos: ChunkPos, _chunk: LoadedChunk) {}
    fn merge(&mut self, _pos: ChunkPos, _patch: ColumnPatch) {}
    fn set_block(&mut self, x: i32, y: i32, z: i32, state: u32) {
        self.set_block.push((x, y, z, state));
    }
    fn set_blocks(
        &mut self,
        _section_x: i32,
        _section_y: i32,
        _section_z: i32,
        _blocks: &[(u8, u8, u8, u32)],
    ) {
        panic!("block_change/multi_block_change route through set_block, not set_blocks");
    }
    fn set_block_entity(&mut self, _x: i32, _y: i32, _z: i32, _type_id: u32, _nbt: Nbt) {}
    fn sync_block_entity(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        block_entity_type: Option<u32>,
    ) -> BlockEntitySync {
        self.sync_block_entity.push((x, y, z, block_entity_type));
        // This sink holds no records, so it cannot report a real outcome; the
        // adapter ignores the value.
        BlockEntitySync::ChunkAbsent
    }
    fn merge_light(&mut self, _pos: ChunkPos, _patch: LightPatch) {}
    fn merge_biomes(&mut self, _pos: ChunkPos, _patch: BiomePatch) {}
    fn unload(&mut self, _pos: ChunkPos) {}
}

/// Independent VarInt encoder (not the codec under test).
fn var_i32(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u32;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}

fn dispatch(
    sink: &mut RecordingSink,
    adapter: &V47Adapter,
    id: i32,
    payload: &[u8],
) -> Vec<lodestone_model::Directive> {
    adapter
        .handle_packet(sink, ConnectionState::Play, id, payload)
        .expect("handle packet")
}

fn assert_remesh(directives: &[lodestone_model::Directive], sections: &[(SectionPos, &[[u8; 3]])]) {
    use lodestone_model::{ClientEvent, Directive};
    assert_eq!(
        directives.len(),
        sections.len(),
        "expected one SectionBlocksChanged directive per touched section, got {directives:?}"
    );
    for directive in directives {
        match directive {
            Directive::Emit(ClientEvent::SectionBlocksChanged { section, blocks }) => {
                let expected = sections
                    .iter()
                    .find(|(want, _)| want == section)
                    .unwrap_or_else(|| panic!("unexpected section {section:?} in {directives:?}"));
                assert_eq!(blocks.as_slice(), expected.1, "wrong cells for section {section:?}");
            }
            other => panic!("expected a SectionBlocksChanged re-mesh directive, got {other:?}"),
        }
    }
}

// ---- block_change ----------------------------------------------------

#[test]
fn block_change_routes_stone_to_canonical_state() {
    let adapter = V47Adapter::new();
    let mut sink = RecordingSink::default();
    // (old_block_id=1 "stone", meta=0) -> composite id 16. Pairwise-distinct
    // coordinates so a packed-position transposition cannot survive.
    let mut payload = pack_position(lodestone_model::BlockPos::new(11, 64, -5))
        .to_be_bytes()
        .to_vec();
    payload.extend_from_slice(&var_i32(16));

    let directives = dispatch(&mut sink, &adapter, play::clientbound::BLOCK_CHANGE, &payload);

    let mut tally = FallbackTally::default();
    let expected_state = canonical::resolve_or_air(1, 0, &mut tally);
    assert!(tally.is_empty(), "stone must resolve cleanly: {tally:?}");
    assert_eq!(sink.set_block, vec![(11, 64, -5, expected_state)]);
    assert_eq!(sink.sync_block_entity, vec![(11, 64, -5, None)]);
    // (11, 64, -5) is section-relative (11, 0, 11) in section (0, 4, -1).
    assert_remesh(&directives, &[(SectionPos::new(0, 4, -1), &[[11, 0, 11]])]);
}

#[test]
fn block_change_rejects_composite_id_outside_the_legacy_table() {
    let adapter = V47Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_position(lodestone_model::BlockPos::new(0, 0, 0))
        .to_be_bytes()
        .to_vec();
    payload.extend_from_slice(&var_i32(0x1000)); // one past the 4095-slot table
    let result = adapter.handle_packet(
        &mut sink,
        ConnectionState::Play,
        play::clientbound::BLOCK_CHANGE,
        &payload,
    );
    assert!(result.is_err(), "an out-of-range composite id must be rejected");
}

#[test]
fn block_change_rejects_trailing_bytes() {
    let adapter = V47Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_position(lodestone_model::BlockPos::new(0, 0, 0))
        .to_be_bytes()
        .to_vec();
    payload.extend_from_slice(&var_i32(16));
    payload.push(0x00);
    let result = adapter.handle_packet(
        &mut sink,
        ConnectionState::Play,
        play::clientbound::BLOCK_CHANGE,
        &payload,
    );
    assert!(result.is_err(), "a misaligned block_change must be rejected");
}

// ---- multi_block_change ------------------------------------------------

#[test]
fn multi_block_change_routes_records_within_one_section() {
    let adapter = V47Adapter::new();
    let mut sink = RecordingSink::default();

    // chunk (2, -3); two records both in y=64 (section y=4): rel (1,64,2) and
    // rel (5,64,9), both "stone" (composite id 16). Pairwise-distinct nibbles
    // so a horizontal-byte transposition cannot survive.
    let mut payload = 2i32.to_be_bytes().to_vec();
    payload.extend_from_slice(&(-3i32).to_be_bytes());
    payload.extend_from_slice(&var_i32(2));
    // record 1: horizontalPos = (x=1)<<4 | (z=2) = 0x12, y=64, blockId=16
    payload.push(0x12);
    payload.push(64);
    payload.extend_from_slice(&var_i32(16));
    // record 2: horizontalPos = (x=5)<<4 | (z=9) = 0x59, y=64, blockId=16
    payload.push(0x59);
    payload.push(64);
    payload.extend_from_slice(&var_i32(16));

    let directives = dispatch(&mut sink, &adapter, play::clientbound::MULTI_BLOCK_CHANGE, &payload);

    let mut tally = FallbackTally::default();
    let expected_state = canonical::resolve_or_air(1, 0, &mut tally);
    assert!(tally.is_empty(), "stone must resolve cleanly: {tally:?}");

    let absolute_1 = (2 * 16 + 1, 64, -3 * 16 + 2);
    let absolute_2 = (2 * 16 + 5, 64, -3 * 16 + 9);
    assert_eq!(
        sink.set_block,
        vec![
            (absolute_1.0, absolute_1.1, absolute_1.2, expected_state),
            (absolute_2.0, absolute_2.1, absolute_2.2, expected_state),
        ]
    );
    assert_remesh(&directives, &[(SectionPos::new(2, 4, -3), &[[1, 0, 2], [5, 0, 9]])]);
}

#[test]
fn multi_block_change_splits_records_spanning_multiple_sections() {
    let adapter = V47Adapter::new();
    let mut sink = RecordingSink::default();

    // chunk (0, 0); one record at y=10 (section 0), one at y=80 (section 5).
    let mut payload = 0i32.to_be_bytes().to_vec();
    payload.extend_from_slice(&0i32.to_be_bytes());
    payload.extend_from_slice(&var_i32(2));
    payload.push(0x00); // rel (0, 0)
    payload.push(10);
    payload.extend_from_slice(&var_i32(16)); // stone
    payload.push(0x00); // rel (0, 0)
    payload.push(80);
    payload.extend_from_slice(&var_i32(16)); // stone

    let directives = dispatch(&mut sink, &adapter, play::clientbound::MULTI_BLOCK_CHANGE, &payload);

    assert_eq!(sink.set_block.len(), 2);
    assert_remesh(
        &directives,
        &[
            (SectionPos::new(0, 0, 0), &[[0, 10, 0]]),
            (SectionPos::new(0, 5, 0), &[[0, 0, 0]]),
        ],
    );
}

#[test]
fn multi_block_change_empty_record_list_emits_nothing() {
    let adapter = V47Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = 0i32.to_be_bytes().to_vec();
    payload.extend_from_slice(&0i32.to_be_bytes());
    payload.extend_from_slice(&var_i32(0));
    let directives = dispatch(&mut sink, &adapter, play::clientbound::MULTI_BLOCK_CHANGE, &payload);
    assert!(directives.is_empty());
    assert!(sink.set_block.is_empty());
}

#[test]
fn multi_block_change_rejects_chunk_coordinate_outside_the_world_border() {
    // Regression fixture for the exact hazard `lodestone-v1-9`'s
    // `chunk_origin_block` guards: a hostile `chunk_x` whose `* 16` would
    // overflow `i32` (or silently wrap in release) must be refused before any
    // record is written, not clamped.
    let adapter = V47Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = 134_217_728i32.to_be_bytes().to_vec(); // chunk_x = i32::MAX/16 + 1
    payload.extend_from_slice(&0i32.to_be_bytes());
    payload.extend_from_slice(&var_i32(0));
    let result = adapter.handle_packet(
        &mut sink,
        ConnectionState::Play,
        play::clientbound::MULTI_BLOCK_CHANGE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a chunk coordinate outside the world border must be rejected"
    );
    assert!(sink.set_block.is_empty());
}
