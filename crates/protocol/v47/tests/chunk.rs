//! Hermetic round-trip and malformed-input tests for the 1.8 (protocol 47)
//! chunk decoder.
//!
//! These build golden 1.8 chunk blobs byte-for-byte and assert the two
//! detectors that catch the subtle failures a length check cannot:
//!
//! * **zero trailing bytes** after decode (`ensure_empty`) — a misparse almost
//!   always leaves the buffer misaligned;
//! * **known block ids at known Y** — a YZX-transposed or big-endian-swapped
//!   decode round-trips byte counts perfectly yet scrambles positions, and only
//!   this assertion catches it.

use lodestone_core::{Reader, Writer};
use lodestone_data::block_states;
use lodestone_v47::packets::chunk::{ChunkShape, MapChunk, MapChunkBulk};

// The wire composites 1.8 actually sends: `(blockId << 4) | meta`. These are
// what the golden blobs below are *built* from.
const WIRE_AIR: u32 = 0; // block id 0, meta 0
const WIRE_BEDROCK: u32 = 7 << 4; // block id 7, meta 0 → composite 112
const WIRE_STONE: u32 = 1 << 4; // block id 1, meta 0 → composite 16

/// The canonical 26.2 block-state id of a property-less block, looked up **by
/// name** in the jar-derived registry.
///
/// The assertions below deliberately do not compare against the wire
/// composite, and equally deliberately do not call the decoder's own
/// canonicalisation to compute what they expect — either would make them
/// vacuous. The registry is an anchor outside this crate entirely; see
/// `canonicalisation.rs` for the gate that gives the mapping itself external
/// provenance.
fn canonical_state(name: &str) -> u32 {
    (0..block_states::STATE_COUNT)
        .find(|&id| {
            block_states::block_name(id) == Some(name) && block_states::properties(id) == Some(&[])
        })
        .unwrap_or_else(|| panic!("26.2 registry defines a property-less {name}"))
}

fn air() -> u32 {
    canonical_state("minecraft:air")
}
fn bedrock() -> u32 {
    canonical_state("minecraft:bedrock")
}
fn stone() -> u32 {
    canonical_state("minecraft:stone")
}

/// Section-local flat index in 1.8 YZX order.
fn idx(x: usize, y: usize, z: usize) -> usize {
    y << 8 | z << 4 | x
}

/// Builds one 16³ section's block data (8192 LE bytes) from a value function.
fn section_block_bytes(mut value_at: impl FnMut(usize, usize, usize) -> u32) -> Vec<u8> {
    let mut out = vec![0u8; 8192];
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                let v = value_at(x, y, z) as u16;
                let i = idx(x, y, z);
                out[2 * i] = (v & 0xFF) as u8;
                out[2 * i + 1] = (v >> 8) as u8;
            }
        }
    }
    out
}

/// Builds a full `map_chunk` body: a single ground-up section (index 0) with
/// bedrock at y=0, stone at y=1, air above; biome footer filled with `biome`.
fn build_map_chunk(x: i32, z: i32, skylight: bool, biome: u8) -> Vec<u8> {
    let blocks = section_block_bytes(|_, y, _| match y {
        0 => WIRE_BEDROCK,
        1 => WIRE_STONE,
        _ => WIRE_AIR,
    });

    let mut blob = Vec::new();
    blob.extend_from_slice(&blocks); // block data (8192)
    blob.extend_from_slice(&[0u8; 2048]); // block light
    if skylight {
        blob.extend_from_slice(&[0xFFu8; 2048]); // sky light
    }
    blob.extend_from_slice(&[biome; 256]); // biome footer

    let mut w = Writer::default();
    w.i32(x);
    w.i32(z);
    w.bool(true); // groundUp
    w.u16(0x0001); // bitmask: section 0 present
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);
    w.into_vec()
}

#[test]
fn decodes_full_chunk_zero_trailing_bytes() {
    let body = build_map_chunk(3, -5, true, 1);
    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty()
        .expect("decode consumes the whole packet (zero trailing bytes)");

    assert_eq!(chunk.x, 3);
    assert_eq!(chunk.z, -5);
    assert!(chunk.ground_up);
    assert_eq!(chunk.column.section_count(), 16);
}

#[test]
fn known_blocks_land_at_known_y() {
    // The transpose / endianness detector: every (x,z) at y=0 must be bedrock,
    // y=1 stone, y=2 air. A YZX-transposed or LE/BE-swapped decode fails here.
    let body = build_map_chunk(0, 0, true, 1);
    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty().expect("aligned");

    let col = &chunk.column;
    for x in 0..16 {
        for z in 0..16 {
            assert_eq!(col.get_block(x, 0, z), bedrock(), "bedrock at y=0 ({x},{z})");
            assert_eq!(col.get_block(x, 1, z), stone(), "stone at y=1 ({x},{z})");
            assert_eq!(col.get_block(x, 2, z), air(), "air at y=2 ({x},{z})");
        }
    }
    // Air far above the single present section is elided (absent) → air.
    assert_eq!(col.get_block(0, 200, 0), air());
}

#[test]
fn light_arrays_decoded_and_placed() {
    let body = build_map_chunk(0, 0, true, 1);
    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty().expect("aligned");

    // Sky light for block-section 0 lives at light section 1, filled 0xFF → 15.
    assert_eq!(chunk.light.sky(1).get(0), Some(15));
    // Block light was all zero.
    assert_eq!(chunk.light.block(1).get(0), Some(0));
}

#[test]
fn biome_footer_downsampled_into_container() {
    let body = build_map_chunk(0, 0, true, 4); // biome id 4 everywhere
    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty().expect("aligned");
    // The section that has blocks carries the down-sampled biome. Column
    // `get_biome` takes 4×4 cell coords in x/z and a world Y.
    assert_eq!(chunk.column.get_biome(0, 0, 0), 4);
    assert_eq!(chunk.column.get_biome(3, 5, 3), 4);
}

#[test]
fn no_skylight_shape_omits_sky_arrays() {
    // In a skyless dimension the sky-light arrays are absent from the blob; the
    // decoder must not try to read them, and the buffer must still be exact.
    let blocks = section_block_bytes(|_, y, _| if y == 0 { WIRE_BEDROCK } else { WIRE_AIR });
    let mut blob = Vec::new();
    blob.extend_from_slice(&blocks);
    blob.extend_from_slice(&[0u8; 2048]); // block light only
    blob.extend_from_slice(&[3u8; 256]); // biomes

    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.u16(0x0001);
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::no_skylight()).expect("decode");
    r.ensure_empty()
        .expect("skyless decode consumes the whole packet");
    assert_eq!(chunk.column.get_block(0, 0, 0), bedrock());
    // No sky light present → the section stays Missing.
    assert_eq!(chunk.light.sky(1).get(0), None);
}

#[test]
fn multiple_present_sections_decode() {
    // Sections 0 and 2 present (bitmask 0b101): section 0 bedrock at its y=0,
    // section 2 stone at its y=0 (world y=32).
    let s0 = section_block_bytes(|_, y, _| if y == 0 { WIRE_BEDROCK } else { WIRE_AIR });
    let s2 = section_block_bytes(|_, y, _| if y == 0 { WIRE_STONE } else { WIRE_AIR });

    let mut blob = Vec::new();
    blob.extend_from_slice(&s0);
    blob.extend_from_slice(&s2);
    blob.extend_from_slice(&[0u8; 2048]); // block light s0
    blob.extend_from_slice(&[0u8; 2048]); // block light s2
    blob.extend_from_slice(&[0xFFu8; 2048]); // sky light s0
    blob.extend_from_slice(&[0xFFu8; 2048]); // sky light s2
    blob.extend_from_slice(&[1u8; 256]); // biomes

    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.u16(0b0000_0000_0000_0101);
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty().expect("aligned");

    assert_eq!(chunk.column.get_block(0, 0, 0), bedrock()); // section 0
    assert_eq!(chunk.column.get_block(0, 32, 0), stone()); // section 2 (world y=32)
    assert_eq!(chunk.column.get_block(0, 16, 0), air()); // section 1 elided
}

#[test]
fn map_chunk_bulk_decodes_each_column() {
    // Two columns, sky light sent. Each: one section (index 0), bedrock at y=0.
    let blocks = section_block_bytes(|_, y, _| if y == 0 { WIRE_BEDROCK } else { WIRE_AIR });
    let one_column_data = |blob: &mut Vec<u8>| {
        blob.extend_from_slice(&blocks);
        blob.extend_from_slice(&[0u8; 2048]); // block light
        blob.extend_from_slice(&[0xFFu8; 2048]); // sky light
        blob.extend_from_slice(&[1u8; 256]); // biomes
    };

    let mut w = Writer::default();
    w.bool(true); // skyLightSent
    w.var_i32(2); // column count
    // metadata for both columns
    w.i32(10);
    w.i32(20);
    w.u16(0x0001);
    w.i32(11);
    w.i32(20);
    w.u16(0x0001);
    // then the concatenated data blob
    let mut blob = Vec::new();
    one_column_data(&mut blob);
    one_column_data(&mut blob);
    w.bytes(&blob);
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let columns = MapChunkBulk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty().expect("bulk consumes the whole packet");

    assert_eq!(columns.len(), 2);
    assert_eq!(columns[0].x, 10);
    assert_eq!(columns[0].z, 20);
    assert_eq!(columns[1].x, 11);
    assert_eq!(columns[0].column.get_block(0, 0, 0), bedrock());
    assert_eq!(columns[1].column.get_block(0, 0, 0), bedrock());
}

#[test]
fn truncated_blob_errors_cleanly() {
    // A declared length longer than the data present must error, not panic.
    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.u16(0x0001);
    w.var_i32(10_000); // claims 10000 bytes
    w.bytes(&[0u8; 100]); // but only 100 present
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let result = MapChunk::decode(&mut r, &ChunkShape::overworld());
    assert!(result.is_err(), "truncated blob must error");
}

#[test]
fn wrong_blob_length_leaves_trailing_bytes() {
    // A blob padded with extra bytes decodes the geometry but fails the
    // zero-trailing-bytes detector — the check that guards against a layout
    // assumption being wrong.
    let blocks = section_block_bytes(|_, y, _| if y == 0 { WIRE_BEDROCK } else { WIRE_AIR });
    let mut blob = Vec::new();
    blob.extend_from_slice(&blocks);
    blob.extend_from_slice(&[0u8; 2048]);
    blob.extend_from_slice(&[0xFFu8; 2048]);
    blob.extend_from_slice(&[1u8; 256]);
    blob.extend_from_slice(&[0u8; 8]); // 8 stray trailing bytes

    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.u16(0x0001);
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let result = MapChunk::decode(&mut r, &ChunkShape::overworld());
    // The bounded sub-reader's ensure_empty inside decode catches the slack.
    assert!(result.is_err(), "extra blob bytes must be rejected");
}
