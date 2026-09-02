//! Hermetic tests for the chunk *seam*: proving that `level_chunk_with_light`
//! and `forget_level_chunk`, handled through the public
//! [`VersionAdapter::handle_packet`] boundary, apply their decoded data to the
//! client-owned [`World`] and surface only a lightweight
//! [`ClientEvent::ChunkLoaded`]/[`ClientEvent::ChunkUnloaded`] notification —
//! not just that the internal decoder works.
//!
//! The live test ([`live_chunk`](../tests/live_chunk.rs)) proves the same seam
//! against real server output; these build synthetic payloads so the wiring is
//! guarded without a network.

use lodestone_core::Writer;
use lodestone_model::adapter::{ConnectionState, Directive, VersionAdapter};
use lodestone_model::event::ClientEvent;
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_v770::packets::chunk::ChunkShape;
use lodestone_world::{ChunkPos, ColumnLight, Heightmaps, PalettedContainer, World};

/// The protocol [`Ctx`](lodestone_v770) used by the world codec is exercised
/// implicitly through the adapter, so the fixture builder mirrors the exact
/// 26.2 framing (two shorts per section, block container before biome
/// container, `FixedSize` long arrays, typed-list heightmaps).
fn encode_chunk_packet(x: i32, z: i32, shape: &ChunkShape, bottom_blocks: &[u32]) -> Vec<u8> {
    let mut w = Writer::default();
    w.i32(x);
    w.i32(z);
    Heightmaps::new().encode(&mut w);

    let mut blob = Writer::default();
    for index in 0..shape.section_count {
        let block_container = if index == 0 {
            PalettedContainer::from_values(shape.block_kind, bottom_blocks)
        } else {
            PalettedContainer::new(shape.block_kind, shape.air_id)
        };
        let non_air = (0..block_container.entry_count())
            .filter(|&i| block_container.get(i) != shape.air_id)
            .count() as i16;
        blob.i16(non_air);
        blob.i16(0);
        block_container.encode(&mut blob);
        PalettedContainer::new(shape.biome_kind, shape.biome_id).encode(&mut blob);
    }
    let blob = blob.into_vec();
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);

    w.var_i32(0); // block entities
    ColumnLight::new(shape.section_count).encode(&mut w);
    w.into_vec()
}

/// A `forget_level_chunk` body is a single packed long: `x` in the low 32 bits,
/// `z` in the high 32 (`vanilla's own chunk pos's own pack`, verified against 26.2 source).
fn encode_forget_packet(x: i32, z: i32) -> Vec<u8> {
    let packed = (x as u32 as i64) | ((z as u32 as i64) << 32);
    let mut w = Writer::default();
    w.i64(packed);
    w.into_vec()
}

#[test]
fn level_chunk_lands_in_world_and_notifies_consumer() {
    let adapter = V770Adapter::new();
    let shape = ChunkShape::overworld_1_21();
    let mut world = World::new();

    // Solid layer of id 1 at local y=0, marker id 7 at (x=1,y=1,z=2).
    let mut blocks = vec![0u32; 4096];
    for slot in blocks.iter_mut().take(256) {
        *slot = 1;
    }
    blocks[256 + 2 * 16 + 1] = 7;

    let payload = encode_chunk_packet(3, -5, &shape, &blocks);
    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
            &payload,
        )
        .expect("adapter handles level_chunk_with_light");

    // The event is a bare notification carrying only the position.
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::ChunkLoaded { pos })] => {
            assert_eq!(pos.x, 3);
            assert_eq!(pos.z, -5);
        }
        other => panic!("expected a single ChunkLoaded notification, got {other:?}"),
    }

    // The data crossed the seam into the client-owned world, keyed by pos.
    let loaded = world
        .get(ChunkPos::new(3, -5))
        .expect("chunk applied to world");
    let column = &loaded.column;
    for x in 0..16 {
        for z in 0..16 {
            assert_eq!(column.get_block(x, -64, z), 1, "solid layer at y=-64");
        }
    }
    assert_eq!(column.get_block(1, -63, 2), 7, "YZX marker preserved");
    assert_eq!(column.get_block(0, 100, 0), 0, "air far above terrain");
}

#[test]
fn forget_level_chunk_removes_from_world_and_notifies_consumer() {
    let adapter = V770Adapter::new();
    let shape = ChunkShape::overworld_1_21();
    let mut world = World::new();

    // Load a chunk first so there is something to forget.
    let load = encode_chunk_packet(-8, 12, &shape, &vec![0u32; 4096]);
    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
            &load,
        )
        .expect("adapter handles level_chunk_with_light");
    assert!(world.contains(ChunkPos::new(-8, 12)), "chunk loaded first");

    let payload = encode_forget_packet(-8, 12);
    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::FORGET_LEVEL_CHUNK,
            &payload,
        )
        .expect("adapter handles forget_level_chunk");

    match directives.as_slice() {
        [Directive::Emit(ClientEvent::ChunkUnloaded { pos })] => {
            assert_eq!(pos.x, -8);
            assert_eq!(pos.z, 12);
        }
        other => panic!("expected a single ChunkUnloaded notification, got {other:?}"),
    }
    assert!(
        !world.contains(ChunkPos::new(-8, 12)),
        "chunk removed from world"
    );
}

#[test]
fn trailing_bytes_in_chunk_payload_error_rather_than_silently_pass() {
    // The seam must be as strict as the white-box decoder was: a misparse that
    // leaves the buffer misaligned has to surface as an error, not a silently
    // truncated chunk. Appending a stray byte to a valid payload must fail, and
    // nothing must be applied to the world.
    let adapter = V770Adapter::new();
    let shape = ChunkShape::overworld_1_21();
    let mut world = World::new();
    let mut payload = encode_chunk_packet(0, 0, &shape, &vec![0u32; 4096]);
    payload.push(0xAB);
    assert!(
        adapter
            .handle_packet(
                &mut world,
                ConnectionState::Play,
                play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
                &payload,
            )
            .is_err(),
        "trailing bytes must be rejected"
    );
    assert!(world.is_empty(), "a rejected chunk must not be applied");
}
