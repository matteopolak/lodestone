//! Hermetic 1.12.2 (protocol 340) chunk-decode tests.
//!
//! These build golden `map_chunk` blobs byte-for-byte and assert the properties
//! that a subtly wrong layout would break:
//!
//! * **zero trailing bytes** after decode (`ensure_empty`) — a misparse almost
//!   always leaves the buffer misaligned, so this is the single best detector;
//! * **known blocks land at known Y** — catches a byte-correct but transposed or
//!   mis-shifted decode, and in particular proves the pre-1.16 **straddling**
//!   long unpacking is correct (a value packed across a 64-bit boundary must be
//!   reconstructed from two longs);
//! * **truncated / over-long payloads are rejected**, not panicked on — the
//!   negative control that proves the zero-trailing-bytes check actually fires.
//!
//! Palettes/direct sections here are built from the **wire's** legacy
//! `(blockId << 4) | meta` composite ids (that is what a real 1.12.2 server
//! sends), but decoded output is asserted against the **canonical 26.2**
//! state ids [`canonical::resolve`] produces — `packets::chunk` now
//! translates every block through `crate::canonical` before it reaches
//! [`lodestone_world`] storage, so a decoded column is never in the wire's
//! id space. See `crate::canonical`'s module docs for why.

use lodestone_core::{Reader, Writer};
use lodestone_v340::canonical::{self, CanonicalBlockState};
use lodestone_v340::packets::chunk::{ChunkShape, MapChunk};

// Wire-format composite ids, used only to build input packet bytes.
const AIR_WIRE: u32 = 0;
const BEDROCK_WIRE: u32 = 7 << 4; // block id 7, meta 0
const STONE_WIRE: u32 = 1 << 4; // block id 1, meta 0

/// The canonical 26.2 state id `crate::canonical::resolve(old_block_id,
/// meta)` produces, for asserting decoded output against.
fn canonical_id(old_block_id: u8, meta: u8) -> u32 {
    match canonical::resolve(old_block_id, meta) {
        CanonicalBlockState::Resolved(id) => id,
        other => panic!("expected a resolved canonical state, got {other:?}"),
    }
}

/// Container index for section-relative `(x, y, z)` in YZX order.
fn idx(x: usize, y: usize, z: usize) -> usize {
    (y << 8) | (z << 4) | x
}

/// Packs `values` at `bits` width using the pre-1.16 **straddling** layout
/// (values cross 64-bit boundaries), mirroring what a 1.12.2 server writes.
fn pack_straddling(values: &[u32], bits: u32) -> Vec<u64> {
    let n = values.len();
    let long_count = (n * bits as usize).div_ceil(64);
    let mut longs = vec![0u64; long_count];
    for (i, &v) in values.iter().enumerate() {
        let bit = i * bits as usize;
        let start = bit / 64;
        let off = (bit % 64) as u32;
        longs[start] |= u64::from(v) << off;
        if off + bits > 64 {
            longs[start + 1] |= u64::from(v) >> (64 - off);
        }
    }
    longs
}

/// Builds one section body: a **direct** (global) palette at 13 bits so the
/// packed indices *are* block-state ids and genuinely straddle longs.
fn direct_section(value_at: impl Fn(usize, usize, usize) -> u32, skylight: bool) -> Vec<u8> {
    let mut values = vec![0u32; 4096];
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                values[idx(x, y, z)] = value_at(x, y, z);
            }
        }
    }
    let bits = 13u32;
    let longs = pack_straddling(&values, bits);

    let mut w = Writer::default();
    w.u8(bits as u8);
    w.var_i32(0); // palette length 0 → direct/global palette
    w.var_i32(longs.len() as i32);
    for long in longs {
        w.i64(long as i64);
    }
    w.bytes(&[0u8; 2048]); // block light
    if skylight {
        w.bytes(&[0xFFu8; 2048]); // sky light
    }
    w.into_vec()
}

/// Builds one section body with an **indirect** palette at 4 bits, exercising
/// the palette-index mapping path (no straddling at 4 bits, which divides 64).
fn indirect_section(skylight: bool) -> Vec<u8> {
    // palette: [AIR, BEDROCK, STONE] (wire composite ids); index 1 → bedrock
    // at y=0, else air.
    let palette = [AIR_WIRE, BEDROCK_WIRE, STONE_WIRE];
    let mut indices = vec![0u32; 4096];
    for z in 0..16 {
        for x in 0..16 {
            indices[idx(x, 0, z)] = 1; // bedrock
            indices[idx(x, 1, z)] = 2; // stone
        }
    }
    let bits = 4u32;
    let longs = pack_straddling(&indices, bits);

    let mut w = Writer::default();
    w.u8(bits as u8);
    w.var_i32(palette.len() as i32);
    for entry in palette {
        w.var_i32(entry as i32);
    }
    w.var_i32(longs.len() as i32);
    for long in longs {
        w.i64(long as i64);
    }
    w.bytes(&[0u8; 2048]);
    if skylight {
        w.bytes(&[0xFFu8; 2048]);
    }
    w.into_vec()
}

/// Wraps a single-section `chunkData` blob into a full `map_chunk` packet.
fn build_map_chunk(x: i32, z: i32, section_body: &[u8], biome: u8, block_entities: i32) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(section_body);
    blob.extend_from_slice(&[biome; 256]); // biome footer (groundUp)

    let mut w = Writer::default();
    w.i32(x);
    w.i32(z);
    w.bool(true); // groundUp
    w.var_i32(0x0001); // bitmask: section 0 present
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);
    w.var_i32(block_entities);
    w.into_vec()
}

#[test]
fn decodes_full_chunk_zero_trailing_bytes() {
    let section = direct_section(|_, y, _| if y == 0 { BEDROCK_WIRE } else { AIR_WIRE }, true);
    let body = build_map_chunk(3, -5, &section, 1, 0);

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
fn straddling_unpack_lands_known_blocks_at_known_y() {
    // Bedrock at y=0, stone at y=1, air above — packed at 13 bits (straddling).
    // If the unpack used the 1.16+ non-straddling layout, these ids scramble.
    let section = direct_section(
        |_, y, _| match y {
            0 => BEDROCK_WIRE,
            1 => STONE_WIRE,
            _ => AIR_WIRE,
        },
        true,
    );
    let body = build_map_chunk(0, 0, &section, 1, 0);

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty().expect("aligned");

    let bedrock = canonical_id(7, 0);
    let stone = canonical_id(1, 0);
    let air = canonical::air_state_id();
    let col = &chunk.column;
    for x in [0usize, 7, 15] {
        for z in [0usize, 9, 15] {
            assert_eq!(col.get_block(x, 0, z), bedrock, "bedrock at y=0 ({x},{z})");
            assert_eq!(col.get_block(x, 1, z), stone, "stone at y=1 ({x},{z})");
            assert_eq!(col.get_block(x, 2, z), air, "air at y=2 ({x},{z})");
        }
    }
}

#[test]
fn indirect_palette_maps_indices() {
    let section = indirect_section(true);
    let body = build_map_chunk(0, 0, &section, 1, 0);

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty().expect("aligned");

    assert_eq!(chunk.column.get_block(0, 0, 0), canonical_id(7, 0));
    assert_eq!(chunk.column.get_block(5, 1, 9), canonical_id(1, 0));
    assert_eq!(chunk.column.get_block(0, 2, 0), canonical::air_state_id());
}

#[test]
fn light_and_biome_decoded() {
    let section = direct_section(|_, y, _| if y == 0 { BEDROCK_WIRE } else { AIR_WIRE }, true);
    let body = build_map_chunk(0, 0, &section, 4, 0); // biome id 4 everywhere

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty().expect("aligned");

    // Section 0 lives at light section 1.
    assert_eq!(chunk.light.sky(1).get(0), Some(15));
    assert_eq!(chunk.light.block(1).get(0), Some(0));
    assert_eq!(chunk.column.get_biome(0, 0, 0), 4);
    assert_eq!(chunk.column.get_biome(3, 5, 3), 4);
}

#[test]
fn no_skylight_shape_omits_sky_arrays() {
    let section = direct_section(|_, y, _| if y == 0 { BEDROCK_WIRE } else { AIR_WIRE }, false);
    let body = build_map_chunk(0, 0, &section, 1, 0);

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::no_skylight()).expect("decode");
    r.ensure_empty()
        .expect("no-skylight geometry consumes the whole packet");

    assert_eq!(chunk.column.get_block(0, 0, 0), canonical_id(7, 0));
    assert_eq!(chunk.light.sky(1).get(0), None);
}

// ---- Negative controls: the check that guards the absence claim must fire. ----

#[test]
fn truncated_blob_errors_cleanly() {
    // A declared chunkData length longer than the bytes present must error.
    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.var_i32(0x0001);
    w.var_i32(10_000); // claims 10000 bytes
    w.bytes(&[0u8; 100]); // but only 100 present
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let result = MapChunk::decode(&mut r, &ChunkShape::overworld());
    assert!(result.is_err(), "truncated blob must error, not panic");
}

#[test]
fn extra_trailing_bytes_rejected() {
    // A valid section padded with stray bytes decodes the geometry but must fail
    // the zero-trailing-bytes detector inside the bounded chunkData sub-reader.
    let section = direct_section(|_, y, _| if y == 0 { BEDROCK_WIRE } else { AIR_WIRE }, true);
    let mut blob = Vec::new();
    blob.extend_from_slice(&section);
    blob.extend_from_slice(&[1u8; 256]); // biomes
    blob.extend_from_slice(&[0u8; 8]); // 8 stray trailing bytes

    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.var_i32(0x0001);
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);
    w.var_i32(0);
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let result = MapChunk::decode(&mut r, &ChunkShape::overworld());
    assert!(result.is_err(), "extra blob bytes must be rejected");
}

#[test]
fn wrong_long_count_rejected() {
    // A data-array length that disagrees with the straddling geometry for the
    // declared bits-per-block must be rejected by the explicit count check.
    let mut section = Vec::new();
    let mut sw = Writer::default();
    sw.u8(13); // 13 bits → expects ceil(4096*13/64)=832 longs
    sw.var_i32(0); // direct palette
    sw.var_i32(800); // WRONG: claims 800 longs
    for _ in 0..800 {
        sw.i64(0);
    }
    sw.bytes(&[0u8; 2048]);
    sw.bytes(&[0xFFu8; 2048]);
    section.extend_from_slice(&sw.into_vec());

    let mut blob = section;
    blob.extend_from_slice(&[1u8; 256]);

    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.var_i32(0x0001);
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);
    w.var_i32(0);
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let result = MapChunk::decode(&mut r, &ChunkShape::overworld());
    assert!(result.is_err(), "wrong long count must be rejected");
}
