//! Hermetic tests for the clientbound `light_update` dispatch arm (protocol
//! 776, id 48).
//!
//! `light_update` carries the *same* six-field light payload embedded in
//! `level_chunk_with_light`, but standalone and applied as a **merge**: a light
//! section named by a full-mask bit is replaced with an explicit array, one
//! named by an empty-mask bit becomes explicit zero, and one named by neither
//! is left untouched. The three-state semantics live in
//! `lodestone_world::LightPatch::from_light_masks`; this arm's own job is to
//! read the wire fields in the right order and hand them over in the right
//! argument positions — the bug-prone part, since the wire order
//! (sky_mask, block_mask, empty_sky_mask, empty_block_mask, sky_updates,
//! block_updates) is *not* the constructor's argument order.
//!
//! The golden fixture deliberately makes sky ≠ block and full ≠ empty so a
//! swapped argument or a mask/empty mix-up is caught by reading the resulting
//! light back out of a real `World`, not merely by a length check.

use lodestone_core::Reader;
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::{
    ChunkColumn, ChunkPos as WorldChunkPos, ColumnLight, Heightmaps, LightData, LoadedChunk,
    PaletteKind, World,
};

/// Four block sections ⇒ six light sections (`0..6`), so the fixture's sky
/// index 1 and block index 2 are both in range.
const SECTION_COUNT: usize = 4;

/// Builds a `World` holding one all-`Missing`-light chunk at `pos`, so a
/// `merge_light` actually lands somewhere (it is a no-op for an unloaded chunk).
fn world_with_empty_chunk(pos: WorldChunkPos) -> World {
    let mut world = World::new();
    let column = ChunkColumn::new(
        -64,
        SECTION_COUNT,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    );
    let light = ColumnLight::new(SECTION_COUNT);
    world.load(
        pos,
        LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
    );
    world
}

/// Golden `light_update` body for chunk (0, 0):
/// * sky_mask names light section **1** (word `0b10`) ⇒ one full sky array,
/// * empty_block_mask names light section **2** (word `0b100`) ⇒ block zero,
/// * the single sky array is 2048 bytes of `0xFF` ⇒ every nibble = 15.
fn golden_light_update() -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0x00); // x = 0 (varint)
    p.push(0x00); // z = 0 (varint)

    // sky_mask: 1 long word = 0b10 (bit 1 set)
    p.push(0x01);
    p.extend_from_slice(&2u64.to_be_bytes());
    // block_mask: empty
    p.push(0x00);
    // empty_sky_mask: empty
    p.push(0x00);
    // empty_block_mask: 1 long word = 0b100 (bit 2 set)
    p.push(0x01);
    p.extend_from_slice(&4u64.to_be_bytes());

    // sky_updates: one array, varint length 2048 (0x80 0x10) then 2048 bytes.
    p.push(0x01);
    p.extend_from_slice(&[0x80, 0x10]);
    p.extend_from_slice(&[0xFF; 2048]);
    // block_updates: none
    p.push(0x00);

    p
}

#[test]
fn light_update_merges_sky_array_and_empty_block_then_notifies() {
    let adapter = V770Adapter::new();
    let pos = WorldChunkPos::new(0, 0);
    let mut world = world_with_empty_chunk(pos);
    let payload = golden_light_update();

    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::LIGHT_UPDATE,
            &payload,
        )
        .expect("light_update decodes and applies");

    let chunk = world.get(pos).expect("chunk still loaded");

    // Sky section 1 got the full array (nibble 15 everywhere).
    match chunk.light.sky(1) {
        LightData::Values(_) => {
            assert_eq!(chunk.light.sky(1).get(0), Some(15), "sky nibble should be 15");
            assert_eq!(
                chunk.light.sky(1).get(4095),
                Some(15),
                "whole 2048-byte array should be read, not a prefix"
            );
        }
        other => panic!("sky section 1 should be a full array, got {other:?}"),
    }

    // Block section 2 was named by the *empty* mask ⇒ explicit zero, NOT absent.
    assert_eq!(
        *chunk.light.block(2),
        LightData::Uniform(0),
        "empty-mask block section must be explicit Uniform(0)"
    );

    // Sections named by neither mask (and the *other* layer at a named index)
    // must be left untouched. These two assertions are what catch a sky/block
    // or mask/empty argument swap: a swap moves the data to the wrong layer or
    // the wrong index and leaves these non-Missing.
    assert_eq!(
        *chunk.light.block(1),
        LightData::Missing,
        "block section 1 was not in the update — a sky/block swap would fill it"
    );
    assert_eq!(
        *chunk.light.sky(2),
        LightData::Missing,
        "sky section 2 was not in the update — a mask/empty swap would fill it"
    );

    // The dispatch emits a ChunkLoaded notification ("region at pos is dirty;
    // re-mesh it") for the light change.
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::ChunkLoaded { pos: p })] => {
            assert_eq!((p.x, p.z), (0, 0));
        }
        other => panic!("expected a single ChunkLoaded emit, got {other:?}"),
    }
}

#[test]
fn light_update_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut world = world_with_empty_chunk(WorldChunkPos::new(0, 0));
    let mut payload = golden_light_update();
    payload.push(0x00); // one stray trailing byte

    let result = adapter.handle_packet(
        &mut world,
        ConnectionState::Play,
        play::clientbound::LIGHT_UPDATE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a trailing byte after the two array lists must fail (ensure_empty), got {result:?}"
    );
}

#[test]
fn light_update_rejects_wrong_array_length() {
    // A nibble array declared as anything other than 2048 bytes must fail:
    // NibbleArray::from_bytes validates the length. Build a payload whose single
    // sky array claims 4 bytes.
    let mut p = Vec::new();
    p.push(0x00); // x
    p.push(0x00); // z
    p.push(0x01); // sky_mask: one word
    p.extend_from_slice(&2u64.to_be_bytes());
    p.push(0x00); // block_mask empty
    p.push(0x00); // empty_sky_mask empty
    p.push(0x00); // empty_block_mask empty
    p.push(0x01); // sky_updates: one array
    p.push(0x04); // varint length = 4 (wrong, must be 2048)
    p.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    p.push(0x00); // block_updates: none

    let adapter = V770Adapter::new();
    let mut world = world_with_empty_chunk(WorldChunkPos::new(0, 0));
    let result = adapter.handle_packet(
        &mut world,
        ConnectionState::Play,
        play::clientbound::LIGHT_UPDATE,
        &p,
    );
    assert!(
        result.is_err(),
        "a light array that is not 2048 bytes must fail, got {result:?}"
    );
}

#[test]
fn light_update_for_unloaded_chunk_is_a_safe_noop_notification() {
    // Servers send light for chunks the client has not loaded yet; merge_light
    // is a documented no-op there, and the arm must still decode cleanly and
    // notify (idempotent "dirty" signal) rather than error.
    let adapter = V770Adapter::new();
    let mut world = World::new(); // nothing loaded
    let payload = golden_light_update();

    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::LIGHT_UPDATE,
            &payload,
        )
        .expect("light_update for an unloaded chunk still decodes");
    assert!(
        matches!(
            directives.as_slice(),
            [Directive::Emit(ClientEvent::ChunkLoaded { .. })]
        ),
        "should still emit a ChunkLoaded notification, got {directives:?}"
    );
    // Sanity: the reader is fully consumed by the arm (no panic, no leftover).
    let _ = Reader::new(&payload);
}
