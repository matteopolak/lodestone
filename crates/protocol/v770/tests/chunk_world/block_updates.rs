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
use lodestone_world::{
    BiomePatch, BlockEntitySync, ChunkPos, ColumnPatch, LightPatch, LoadedChunk, World, WorldSink,
};

/// A [`WorldSink`] that records single- and multi-block writes for assertion.
#[derive(Default)]
struct RecordingSink {
    set_block: Vec<(i32, i32, i32, u32)>,
    set_blocks: Vec<SectionWrite>,
    set_block_entity: Vec<(i32, i32, i32, u32, Nbt)>,
    sync_block_entity: Vec<(i32, i32, i32, Option<u32>)>,
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
    fn merge_biomes(&mut self, _pos: ChunkPos, _patch: BiomePatch) {}
    fn unload(&mut self, _pos: ChunkPos) {}
    fn set_block_entity(&mut self, x: i32, y: i32, z: i32, type_id: u32, nbt: Nbt) {
        self.set_block_entity.push((x, y, z, type_id, nbt));
    }
    fn sync_block_entity(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        block_entity_type: Option<u32>,
    ) -> BlockEntitySync {
        self.sync_block_entity.push((x, y, z, block_entity_type));
        // This sink holds no records, so it cannot report a real outcome; the
        // adapter ignores the value, and the *world-backed* gates below are what
        // prove the four real outcomes.
        BlockEntitySync::ChunkAbsent
    }
}

/// Independently packs a `BlockPos` the way vanilla `vanilla's own block pos's own as long` does:
/// `x` in bits 38–63, `z` in bits 12–37, `y` in bits 0–11.
fn pack_block_pos(x: i32, y: i32, z: i32) -> i64 {
    ((i64::from(x) & 0x3FF_FFFF) << 38)
        | ((i64::from(z) & 0x3FF_FFFF) << 12)
        | (i64::from(y) & 0xFFF)
}

/// Independently packs a `SectionPos` the way vanilla `vanilla's own section pos's own as long` does:
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

fn dispatch(
    sink: &mut RecordingSink,
    adapter: &V770Adapter,
    id: i32,
    payload: &[u8],
) -> Vec<lodestone_model::Directive> {
    adapter
        .handle_packet(sink, ConnectionState::Play, id, payload)
        .expect("handle packet")
}

/// Asserts the single directive a world-mutating packet emits is a
/// `SectionBlocksChanged` dirty-region signal naming the given column and the
/// given section-relative cells.
///
/// This deliberately does *not* accept `ChunkLoaded`. `7725aa3` split the two
/// signals apart because they are different invalidation units: a column
/// arrival dirties the column and its 8 horizontal seams, while a block change
/// dirties one section and only the neighbours the changed cell physically
/// touches. Conflating them cost ~216 section meshes per redstone tick.
///
/// The section-relative coordinates are load-bearing, not decoration — they are
/// what lets a consumer tell an interior edit from a boundary one, so they are
/// asserted rather than ignored.
fn assert_remesh(
    directives: &[lodestone_model::Directive],
    chunk_x: i32,
    chunk_z: i32,
    blocks: &[[u8; 3]],
) {
    use lodestone_model::{ClientEvent, Directive};
    match directives {
        [Directive::Emit(ClientEvent::SectionBlocksChanged {
            section,
            blocks: got,
        })] => {
            assert_eq!(
                (section.x, section.z),
                (chunk_x, chunk_z),
                "block change must dirty its owning column"
            );
            assert_eq!(
                got.as_slice(),
                blocks,
                "block change must name the section-relative cells it touched"
            );
        }
        other => panic!("expected one SectionBlocksChanged re-mesh directive, got {other:?}"),
    }
}

// ---- block_update ---------------------------------------------------------

#[test]
fn block_update_routes_single_set_block() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_block_pos(10, -5, 20).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(100)); // block state id 100

    let directives = dispatch(
        &mut sink,
        &adapter,
        play::clientbound::BLOCK_UPDATE,
        &payload,
    );
    assert_eq!(sink.set_block, vec![(10, -5, 20, 100)]);
    assert!(sink.set_blocks.is_empty());
    // block (10, -5, 20) lives in chunk column (0, 1).
    assert_remesh(&directives, 0, 1, &[[10, 11, 4]]);
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

    let directives = dispatch(
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
    // section (1, -2, 3) shares its chunk column (1, 3).
    assert_remesh(&directives, 1, 3, &[[1, 2, 3], [15, 0, 15]]);
}

#[test]
fn section_blocks_update_empty_change_set_is_a_noop_write() {
    let adapter = V770Adapter::new();
    let mut sink = RecordingSink::default();
    let mut payload = pack_section_pos(0, 0, 0).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(0)); // zero changes

    let directives = dispatch(
        &mut sink,
        &adapter,
        play::clientbound::SECTION_BLOCKS_UPDATE,
        &payload,
    );
    assert_eq!(sink.set_blocks, vec![(0, 0, 0, vec![])]);
    // An empty change set touched nothing, so it must not force a re-mesh.
    assert!(
        directives.is_empty(),
        "empty section update emits no re-mesh, got {directives:?}"
    );
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

    let directives = dispatch(
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
    // Block-entity data is not block geometry, so it drives no cube re-mesh.
    assert!(
        directives.is_empty(),
        "block_entity_data emits no re-mesh, got {directives:?}"
    );
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

// ---- block-state writes create and remove block entities -------------------
//
// The three tests above prove the adapter *records* a `sync_block_entity` call.
// That is not the claim that matters: a recording sink cannot tell a correct
// type id from a wrong one, and cannot show a record appearing. So these gates
// dispatch the same real packets into a real [`World`] holding a real loaded
// chunk, and read the resulting `LoadedChunk::block_entities` — the exact field
// `lodestone-shell`'s `block_entities::chest_spawns` iterates.
//
// The chunk really being loaded is the load-bearing precondition, and it is the
// `world` species of vacuous test if it is not: every seam here is documented as
// a silent no-op for an absent column, so a fixture that forgot to `load` would
// see zero records and read as a broken feature. `world_with_chunk` is therefore
// asserted non-empty of *sections* and each test asserts the state write landed
// before it asserts anything about block entities.

/// A `World` holding one loaded, all-air chunk at `pos`, deep enough for
/// `y = 64`.
fn world_with_chunk(pos: ChunkPos) -> World {
    use lodestone_world::{ChunkColumn, ColumnLight, Heightmaps, PaletteKind};
    let mut world = World::new();
    let column = ChunkColumn::new(-64, 24, PaletteKind::block_states(), PaletteKind::biomes(), 0, 0);
    world.load(
        pos,
        LoadedChunk::new(column, ColumnLight::new(26), Heightmaps::new(), Vec::new()),
    );
    world
}

/// The first block-state id of a named block, from the real 26.2 census.
///
/// Never a hardcoded state id: those shift with every data bump, and a stale one
/// is the classic silently-passing fixture.
fn first_state_named(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the 26.2 block-state table"))
}

fn block_entity_types_at(world: &World, pos: ChunkPos) -> Vec<(u8, i16, u8, u32)> {
    world
        .get(pos)
        .expect("chunk loaded")
        .block_entities
        .iter()
        .map(|be| (be.rel_x, be.y, be.rel_z, be.type_id))
        .collect()
}

fn block_update_payload(x: i32, y: i32, z: i32, state: u32) -> Vec<u8> {
    let mut payload = pack_block_pos(x, y, z).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(state as i32));
    payload
}

/// A `block_update` carrying a chest state must create the block entity, with
/// the type id the census names — and a following `block_update` carrying air
/// must remove it.
///
/// Both directions in one test on purpose: the removal half is only meaningful
/// against a record that provably existed a moment earlier.
#[test]
fn a_block_update_creates_and_then_removes_a_chests_block_entity() {
    let adapter = V770Adapter::new();
    let pos = ChunkPos::new(0, 0);
    let mut world = world_with_chunk(pos);
    let chest = first_state_named("minecraft:chest");
    let air = first_state_named("minecraft:air");
    let chest_type = lodestone_data::block_entity_types::block_entity_type(chest)
        .expect("a chest state owns a block entity");

    assert!(
        block_entity_types_at(&world, pos).is_empty(),
        "the fixture chunk must start with no block entities"
    );

    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::BLOCK_UPDATE,
            &block_update_payload(3, 64, 9, chest),
        )
        .expect("handle block_update");

    // The precondition, measured: the state write itself landed. Without this an
    // unloaded-chunk fixture would make every assertion below vacuous.
    assert_eq!(
        world.get(pos).expect("chunk").column.get_block(3, 64, 9),
        chest,
        "the block state must have been written — if not, the chunk was not loaded \
         and this gate proves nothing"
    );
    assert_eq!(
        block_entity_types_at(&world, pos),
        vec![(3, 64, 9, chest_type)],
        "a placed chest must gain a block-entity record from the state alone, with \
         no block_entity_data packet"
    );

    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::BLOCK_UPDATE,
            &block_update_payload(3, 64, 9, air),
        )
        .expect("handle block_update");

    assert_eq!(
        world.get(pos).expect("chunk").column.get_block(3, 64, 9),
        air
    );
    assert!(
        block_entity_types_at(&world, pos).is_empty(),
        "breaking the chest must drop the record, or the renderer draws a chest in \
         empty air: {:?}",
        block_entity_types_at(&world, pos)
    );
}

/// Plain terrain must not manufacture records. This is the control on the
/// census's `None` sentinel: with a `0` sentinel every stone block would gain a
/// `minecraft:furnace`, and the create half above would still pass.
#[test]
fn a_block_update_carrying_plain_terrain_creates_nothing() {
    let adapter = V770Adapter::new();
    let pos = ChunkPos::new(0, 0);
    let mut world = world_with_chunk(pos);
    let stone = first_state_named("minecraft:stone");

    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::BLOCK_UPDATE,
            &block_update_payload(3, 64, 9, stone),
        )
        .expect("handle block_update");

    assert_eq!(
        world.get(pos).expect("chunk").column.get_block(3, 64, 9),
        stone,
        "the state write must have landed for this absence to mean anything"
    );
    assert!(
        block_entity_types_at(&world, pos).is_empty(),
        "stone owns no block entity: {:?}",
        block_entity_types_at(&world, pos)
    );
}

/// `section_blocks_update` is the bulk path a piston, a `/fill` or a falling tree
/// arrives on — it is *not* N `block_update`s, so it needs its own proof.
///
/// One packet places two chests and clears a third cell, all in one section, and
/// the per-cell coordinate reconstruction (`section << 4 | rel`) is what is really
/// under test: getting it wrong puts the record 16 blocks away, where it still
/// exists and still fails to draw.
#[test]
fn a_section_blocks_update_creates_and_removes_per_cell() {
    let adapter = V770Adapter::new();
    let pos = ChunkPos::new(-1, 2);
    let mut world = world_with_chunk(pos);
    let chest = first_state_named("minecraft:chest");
    let air = first_state_named("minecraft:air");
    let chest_type = lodestone_data::block_entity_types::block_entity_type(chest).expect("type");

    // Seed a record that the packet's air cell must remove.
    world.sync_block_entity(-16, 64, 34, Some(chest_type));
    assert_eq!(block_entity_types_at(&world, pos), vec![(0, 64, 2, chest_type)]);

    // Section (-1, 4, 2) covers blocks x -16..0, y 64..80, z 32..48.
    let mut payload = pack_section_pos(-1, 4, 2).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(3));
    for (rel_x, rel_y, rel_z, state) in [(0u8, 0u8, 2u8, air), (5, 1, 7, chest), (9, 0, 11, chest)] {
        let local = (i64::from(rel_x) << 8) | (i64::from(rel_z) << 4) | i64::from(rel_y);
        payload.extend_from_slice(&var_i64((i64::from(state) << 12) | local));
    }
    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::SECTION_BLOCKS_UPDATE,
            &payload,
        )
        .expect("handle section_blocks_update");

    let column = &world.get(pos).expect("chunk").column;
    assert_eq!(column.get_block(5, 65, 7), chest, "the bulk state writes landed");
    assert_eq!(column.get_block(0, 64, 2), air);

    let mut records = block_entity_types_at(&world, pos);
    records.sort_unstable();
    assert_eq!(
        records,
        vec![(5, 65, 7, chest_type), (9, 64, 11, chest_type)],
        "both chests in the batch must gain records at their own cells, and the \
         cleared cell must lose its one"
    );
}

/// The negative-coordinate case for the bulk path's `section << 4 | rel`
/// reconstruction, which is where an arithmetic slip hides: section x `-1`,
/// rel `15` is block `-1`, not `-16 - 15`.
#[test]
fn a_section_blocks_update_reconstructs_negative_coordinates() {
    let adapter = V770Adapter::new();
    let pos = ChunkPos::new(-1, -1);
    let mut world = world_with_chunk(pos);
    let chest = first_state_named("minecraft:chest");
    let chest_type = lodestone_data::block_entity_types::block_entity_type(chest).expect("type");

    // Section (-1, -1, -1) covers blocks x/y/z -16..0.
    let mut payload = pack_section_pos(-1, -1, -1).to_be_bytes().to_vec();
    payload.extend_from_slice(&var_i32(1));
    let local = (15i64 << 8) | (13i64 << 4) | 3i64; // rel_x 15, rel_z 13, rel_y 3
    payload.extend_from_slice(&var_i64((i64::from(chest) << 12) | local));
    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::SECTION_BLOCKS_UPDATE,
            &payload,
        )
        .expect("handle section_blocks_update");

    assert_eq!(
        world.get(pos).expect("chunk").column.get_block(15, -13, 13),
        chest,
        "the state write must land at block (-1, -13, -3)"
    );
    assert_eq!(
        block_entity_types_at(&world, pos),
        vec![(15, -13, 13, chest_type)],
        "the record must be keyed by the same cell the state was written to"
    );
}

/// A `block_entity_data` for a chest whose state is already set must **not** wipe
/// the record's payload on the next `block_update` that repeats the same state.
///
/// This is vanilla's `isValidBlockState` branch (`vanilla's own level chunk's own java`): a
/// same-type sync keeps the existing entity. A chest's contents arrive by
/// `block_entity_data`, and the server re-sends `block_update` for a chest
/// whenever its `type`/`facing` changes (a neighbouring chest making it a double),
/// so getting this wrong empties chests as the player builds next to them.
#[test]
fn a_repeated_block_update_keeps_the_nbt_block_entity_data_delivered() {
    let adapter = V770Adapter::new();
    let pos = ChunkPos::new(0, 0);
    let mut world = world_with_chunk(pos);
    let chest = first_state_named("minecraft:chest");

    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::BLOCK_UPDATE,
            &block_update_payload(3, 64, 9, chest),
        )
        .expect("handle block_update");

    // The server's payload for that chest.
    let mut data = pack_block_pos(3, 64, 9).to_be_bytes().to_vec();
    let chest_type = lodestone_data::block_entity_types::block_entity_type(chest).expect("type");
    data.extend_from_slice(&var_i32(chest_type as i32));
    data.extend_from_slice(&empty_compound());
    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::BLOCK_ENTITY_DATA,
            &data,
        )
        .expect("handle block_entity_data");
    assert_eq!(
        world.get(pos).expect("chunk").block_entities[0].nbt,
        Nbt::Compound(vec![]),
        "block_entity_data must reach the record the state write created"
    );

    // The same state again: keep, do not replace.
    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::BLOCK_UPDATE,
            &block_update_payload(3, 64, 9, chest),
        )
        .expect("handle block_update");
    assert_eq!(
        world.get(pos).expect("chunk").block_entities,
        vec![lodestone_world::BlockEntity {
            rel_x: 3,
            rel_z: 9,
            y: 64,
            type_id: chest_type,
            nbt: Nbt::Compound(vec![]),
        }],
        "a same-type state write must keep the record and its NBT, not recreate it"
    );
}
