//! Hermetic framing tests for `minecraft:chunks_biomes` (id `13`).
//!
//! Vanilla's own clientbound chunks-biomes packet (confirmed against the
//! decompiled 26.2 source) carries a VarInt-prefixed
//! list of `(ChunkPos, byte[])` entries, where each byte array is, per
//! `vanilla's own chunk biome data's own extract chunk data`, every section's
//! biome-container encoder output **back to back with no other
//! framing** — no non-air/fluid counts, no block-state container, just
//! `section_count` biome containers in ascending section order. `ChunkPos` is
//! `readChunkPos`/`vanilla's own chunk pos's own pack`: a raw `i64` with `x` in the low 32 bits and
//! `z` in the high 32, the same layout `forget_level_chunk` (id `0x21` at this
//! protocol) already unpacks.
//!
//! Vanilla's only sender is `vanilla's own chunk map's own resend biomes for chunks`, whose only
//! caller is `FillBiomeCommand` (`/fillbiome`) — it *updates* a chunk a player
//! already has loaded; the client never needs it to *create* one, which is why
//! [`lodestone_world::World::merge_biomes`] is a no-op for an absent chunk. See
//! `crates/versions/26.2/src/adapter.rs`'s `CHUNKS_BIOMES` arm and
//! `docs/worldgen-biomes.md`.

use lodestone_core::Writer;
use lodestone_model::{ChunkPos, ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::packets::chunk::ChunkShape;
use lodestone_world::{ChunkPos as WorldChunkPos, PalettedContainer, World};

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

/// `vanilla's own chunk pos's own pack`: x in the low 32 bits, z in the high 32.
fn pack_chunk_pos(x: i32, z: i32) -> i64 {
    (i64::from(x) & 0xFFFF_FFFF) | (i64::from(z) << 32)
}

/// Builds one `ChunkBiomeData` entry's byte array: `section_count` biome
/// containers back to back, where section `marked_section` is single-valued
/// `marked_biome` and every other section is single-valued `default_biome`.
fn encode_chunk_biome_data(shape: &ChunkShape, marked_section: usize, marked_biome: u32) -> Vec<u8> {
    let mut w = Writer::default();
    for index in 0..shape.section_count {
        let value = if index == marked_section {
            marked_biome
        } else {
            shape.biome_id
        };
        PalettedContainer::new(shape.biome_kind, value).encode(&mut w);
    }
    w.into_vec()
}

/// Builds a full `chunks_biomes` packet body naming one chunk.
fn encode_packet(shape: &ChunkShape, x: i32, z: i32, marked_section: usize, marked_biome: u32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(1); // one chunk in this packet
    w.i64(pack_chunk_pos(x, z));
    let blob = encode_chunk_biome_data(shape, marked_section, marked_biome);
    w.bytes(&var_i32(blob.len() as i32));
    w.bytes(&blob);
    w.into_vec()
}

/// A pre-loaded chunk with real block data, so the test can prove a biomes-only
/// update leaves it untouched.
fn loaded_world_with(pos: ChunkPos, shape: &ChunkShape) -> World {
    let mut world = World::new();
    let mut column = lodestone_world::ChunkColumn::new(
        shape.min_y,
        shape.section_count,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );
    column.set_block(1, 5, 1, 77);
    let light = lodestone_world::ColumnLight::new(shape.section_count);
    let mut heightmaps = lodestone_world::Heightmaps::new();
    heightmaps.insert(0, lodestone_world::Heightmap::new(shape.world_height));
    world.load(
        WorldChunkPos::new(pos.x, pos.z),
        lodestone_world::LoadedChunk::new(column, light, heightmaps, Vec::new()),
    );
    world
}

#[test]
fn chunks_biomes_overwrites_the_named_section_and_leaves_blocks_untouched() {
    let shape = ChunkShape::overworld_1_21();
    let pos = ChunkPos::new(3, -5);
    let mut world = loaded_world_with(pos, &shape);

    // World y = 0 is section 4 for min_y = -64 (`(0 - -64) / 16 = 4`), the same
    // section the block write above (y = 5) lands in — so this proves the
    // biome overwrite lands in the section a real block write already
    // occupies, not just an arbitrary empty one.
    let payload = encode_packet(&shape, pos.x, pos.z, 4, 9);

    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::CHUNKS_BIOMES,
            &payload,
        )
        .expect("handle_packet decodes chunks_biomes");

    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ChunkLoaded { pos })],
        "a biomes update must dirty the column for a remesh, exactly as light_update does"
    );

    let column = &world.get(WorldChunkPos::new(pos.x, pos.z)).unwrap().column;
    assert_eq!(
        column.get_biome(0, 0, 0),
        9,
        "the named section's biome was overwritten"
    );
    assert_eq!(
        column.get_block(1, 5, 1),
        77,
        "a biomes-only packet must never touch block state"
    );
}

#[test]
fn chunks_biomes_is_a_noop_for_a_chunk_the_client_does_not_hold() {
    // Vanilla only ever sends this for a chunk a player already has loaded
    // (`vanilla's own chunk map's own resend biomes for chunks` iterates `getPlayers`), so a chunk we
    // do not hold must be dropped rather than fabricated from biomes alone —
    // biomes carry no shape (min-Y, section count, palettes) to build one from.
    let shape = ChunkShape::overworld_1_21();
    let mut world = World::new();
    let payload = encode_packet(&shape, 100, 100, 0, 9);

    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::CHUNKS_BIOMES,
            &payload,
        )
        .expect("handle_packet decodes chunks_biomes");

    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ChunkLoaded {
            pos: ChunkPos::new(100, 100)
        })],
        "the dirty-region signal still fires even though there is nothing to dirty"
    );
    assert!(
        world.is_empty(),
        "merge_biomes must not synthesise a chunk from a biomes-only update"
    );
}

#[test]
fn chunks_biomes_rejects_trailing_bytes() {
    let shape = ChunkShape::overworld_1_21();
    let mut payload = encode_packet(&shape, 0, 0, 0, 3);
    payload.push(0xFF); // one stray byte past the declared framing

    let mut world = World::new();
    let adapter = V770Adapter::new();
    let result = adapter.handle_packet(
        &mut world,
        ConnectionState::Play,
        play::clientbound::CHUNKS_BIOMES,
        &payload,
    );
    assert!(
        result.is_err(),
        "a trailing byte must be rejected, not silently ignored"
    );
}

#[test]
fn chunks_biomes_handles_multiple_chunks_in_one_packet() {
    let shape = ChunkShape::overworld_1_21();
    let a = ChunkPos::new(0, 0);
    let b = ChunkPos::new(1, 0);
    let mut world = World::new();
    for pos in [a, b] {
        let column = lodestone_world::ChunkColumn::new(
            shape.min_y,
            shape.section_count,
            shape.block_kind,
            shape.biome_kind,
            shape.air_id,
            shape.biome_id,
        );
        let light = lodestone_world::ColumnLight::new(shape.section_count);
        let mut heightmaps = lodestone_world::Heightmaps::new();
        heightmaps.insert(0, lodestone_world::Heightmap::new(shape.world_height));
        world.load(
            WorldChunkPos::new(pos.x, pos.z),
            lodestone_world::LoadedChunk::new(column, light, heightmaps, Vec::new()),
        );
    }

    let mut w = Writer::default();
    w.var_i32(2);
    w.i64(pack_chunk_pos(a.x, a.z));
    let blob_a = encode_chunk_biome_data(&shape, 4, 5);
    w.bytes(&var_i32(blob_a.len() as i32));
    w.bytes(&blob_a);
    w.i64(pack_chunk_pos(b.x, b.z));
    let blob_b = encode_chunk_biome_data(&shape, 4, 6);
    w.bytes(&var_i32(blob_b.len() as i32));
    w.bytes(&blob_b);
    let payload = w.into_vec();

    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::CHUNKS_BIOMES,
            &payload,
        )
        .expect("handle_packet decodes chunks_biomes");

    assert_eq!(
        directives,
        vec![
            Directive::Emit(ClientEvent::ChunkLoaded { pos: a }),
            Directive::Emit(ClientEvent::ChunkLoaded { pos: b }),
        ]
    );
    assert_eq!(
        world
            .get(WorldChunkPos::new(a.x, a.z))
            .unwrap()
            .column
            .get_biome(0, 0, 0),
        5
    );
    assert_eq!(
        world
            .get(WorldChunkPos::new(b.x, b.z))
            .unwrap()
            .column
            .get_biome(0, 0, 0),
        6
    );
}
