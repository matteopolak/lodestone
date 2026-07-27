//! Hermetic tests for the protocol 776 `block_update`, `section_blocks_update`,
//! and `block_entity_data` packets.
//!
//! These packets mutate the client-owned world rather than emitting events, so
//! the assertions run against a recording [`WorldSink`] that captures the exact
//! `set_block` / `set_blocks` calls the adapter routes. Golden byte vectors are
//! hand-assembled from the wire specification (behavioural reference only): a
//! packed `BlockPos`/`SectionPos` long plus VarInt/VarLong bodies, so a
//! symmetric encode/decode bug cannot pass silently.

use lodestone_core::Nbt;
use lodestone_model::{ConnectionState, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::{ChunkPos, ColumnPatch, LightPatch, LoadedChunk, WorldSink};

/// A [`WorldSink`] that records single- and multi-block writes for assertion.
#[derive(Default)]
struct RecordingSink {
    set_block: Vec<(i32, i32, i32, u32)>,
    set_blocks: Vec<SectionWrite>,
    set_block_entity: Vec<(i32, i32, i32, u32, Nbt)>,
}

/// One recorded `set_blocks` call: the section grid coordinates and the
/// section-relative block writes applied to it.
type SectionWrite = (i32, i32, i32, Vec<(u8, u8, u8, u32)>);

impl WorldSink for RecordingSink {
    fn load(&mut self, _pos: ChunkPos, _chunk: LoadedChunk) {}
    fn merge(&mut self, _pos: ChunkPos, _patch: ColumnPatch) {}
    fn set_block(&mut self, x: i32, y: i32, z: i32, state: u32) {
        self.set_block.push((x, y, z, state));
    }
    fn set_blocks(
        &mut self,
        section_x: i32,
        section_y: i32,
        section_z: i32,
        blocks: &[(u8, u8, u8, u32)],
    ) {
        self.set_blocks
            .push((section_x, section_y, section_z, blocks.to_vec()));
    }
    fn merge_light(&mut self, _pos: ChunkPos, _patch: LightPatch) {}
    fn unload(&mut self, _pos: ChunkPos) {}
    fn set_block_entity(&mut self, x: i32, y: i32, z: i32, type_id: u32, nbt: Nbt) {
        self.set_block_entity.push((x, y, z, type_id, nbt));
    }
}

/// Independently packs a `BlockPos` the way vanilla `BlockPos.asLong` does:
/// `x` in bits 38–63, `z` in bits 12–37, `y` in bits 0–11.
fn pack_block_pos(x: i32, y: i32, z: i32) -> i64 {
    ((i64::from(x) & 0x3FF_FFFF) << 38)
        | ((i64::from(z) & 0x3FF_FFFF) << 12)
        | (i64::from(y) & 0xFFF)
}

/// Independently packs a `SectionPos` the way vanilla `SectionPos.asLong` does:
/// `x` in bits 42–63, `z` in bits 20–41, `y` in bits 0–19.
fn pack_section_pos(x: i32, y: i32, z: i32) -> i64 {
    ((i64::from(x) & 0x3F_FFFF) << 42)
        | ((i64::from(z) & 0x3F_FFFF) << 20)
        | (i64::from(y) & 0xF_FFFF)
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

/// Independent VarLong encoder (not the codec under test).
fn var_i64(value: i64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u64;
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

fn dispatch(sink: &mut RecordingSink, adapter: &V770Adapter, id: i32, payload: &[u8]) {
    let directives = adapter
        .handle_packet(sink, ConnectionState::Play, id, payload)
        .expect("handle packet");
    assert!(
        directives.is_empty(),
        "world-mutating packets emit no directives, got {directives:?}"
    );
}

// ---- block_update ---------------------------------------------------------

#[test]
fn block_update_routes_single_set_block() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_block_pos(10, -5, 20).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(100)); // block state id 100

    dispatch(
        &mut sink,
        &adapter,
        play::clientbound::BLOCK_UPDATE,
        &payload,
    );
    assert_eq!(sink.set_block, vec![(10, -5, 20, 100)]);
    assert!(sink.set_blocks.is_empty());
}

#[test]
fn block_update_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_block_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(1));
    payload.push(0x00);
    let result = adapter.handle_packet(
        &mut sink,
        ConnectionState::Play,
        play::clientbound::BLOCK_UPDATE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned block_update must be rejected"
    );
}

#[test]
fn block_update_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let payload = pack_block_pos(0, 0, 0).to_be_bytes().to_vec(); // missing state id
    let result = adapter.handle_packet(
        &mut sink,
        ConnectionState::Play,
        play::clientbound::BLOCK_UPDATE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a truncated block_update must be rejected, not panic"
    );
}

// ---- section_blocks_update ------------------------------------------------

#[test]
fn section_blocks_update_routes_relative_writes() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();

    let mut payload = pack_section_pos(1, -2, 3).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(2)); // two changes
    // (relX=1, relY=2, relZ=3) -> local = 1<<8 | 3<<4 | 2 = 306, state 100.
    payload.extend_from_slice(&var_i64((100i64 << 12) | 306));
    // (relX=15, relY=0, relZ=15) -> local = 15<<8 | 15<<4 | 0 = 4080, state 1.
    payload.extend_from_slice(&var_i64((1i64 << 12) | 4080));

    dispatch(
        &mut sink,
        &adapter,
        play::clientbound::SECTION_BLOCKS_UPDATE,
        &payload,
    );
    assert_eq!(
        sink.set_blocks,
        vec![(1, -2, 3, vec![(1, 2, 3, 100), (15, 0, 15, 1)])]
    );
    assert!(sink.set_block.is_empty());
}

#[test]
fn section_blocks_update_empty_change_set_is_a_noop_write() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_section_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(0)); // zero changes

    dispatch(
        &mut sink,
        &adapter,
        play::clientbound::SECTION_BLOCKS_UPDATE,
        &payload,
    );
    assert_eq!(sink.set_blocks, vec![(0, 0, 0, vec![])]);
}

#[test]
fn section_blocks_update_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_section_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&var_i64((5i64 << 12) | 1));
    payload.push(0x00);
    let result = adapter.handle_packet(
        &mut sink,
        ConnectionState::Play,
        play::clientbound::SECTION_BLOCKS_UPDATE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned section_blocks_update must be rejected"
    );
}

#[test]
fn section_blocks_update_rejects_truncated_change_list() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_section_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(3)); // claims 3 but supplies one
    payload.extend_from_slice(&var_i64((5i64 << 12) | 1));
    let result = adapter.handle_packet(
        &mut sink,
        ConnectionState::Play,
        play::clientbound::SECTION_BLOCKS_UPDATE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a truncated change list must be rejected, not panic"
    );
}

// ---- block_entity_data ------------------------------------------------

/// Nameless network NBT for an empty compound: `TAG_Compound` root, then
/// immediately `TAG_End`.
fn empty_compound() -> Vec<u8> {
    vec![0x0A, 0x00]
}

#[test]
fn block_entity_data_routes_set_block_entity() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_block_pos(5, 64, -8).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(1)); // block entity type id 1 (unresolved raw id; carried opaque)
    payload.extend_from_slice(&empty_compound());

    dispatch(
        &mut sink,
        &adapter,
        play::clientbound::BLOCK_ENTITY_DATA,
        &payload,
    );
    assert_eq!(
        sink.set_block_entity,
        vec![(5, 64, -8, 1, Nbt::Compound(vec![]))]
    );
    assert!(sink.set_block.is_empty());
    assert!(sink.set_blocks.is_empty());
}

#[test]
fn block_entity_data_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_block_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(0));
    payload.extend_from_slice(&empty_compound());
    payload.push(0x00);
    let result = adapter.handle_packet(
        &mut sink,
        ConnectionState::Play,
        play::clientbound::BLOCK_ENTITY_DATA,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned block_entity_data must be rejected"
    );
}

#[test]
fn block_entity_data_rejects_truncated_nbt() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_block_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(0));
    payload.push(0x0A); // compound tag id, but no TAG_End follows
    let result = adapter.handle_packet(
        &mut sink,
        ConnectionState::Play,
        play::clientbound::BLOCK_ENTITY_DATA,
        &payload,
    );
    assert!(
        result.is_err(),
        "a truncated block_entity_data must be rejected, not panic"
    );
}
