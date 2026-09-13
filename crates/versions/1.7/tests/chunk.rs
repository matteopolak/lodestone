//! Hermetic tests for the protocol 5 chunk decoder, built from golden blobs.
//!
//! # Why these exist alongside the real capture
//!
//! `capture_join.rs` replays a real five-column `map_chunk_bulk` and is the
//! authority for the column layout. It cannot reach three things:
//!
//! * the **single-column `map_chunk` framing**, which a vanilla server never
//!   sends during a join — its initial world send is always bulk;
//! * the **chunk-unload signal**, which is a `map_chunk` with an empty primary
//!   bitmask and a zlib stream of nothing;
//! * the **add array**, which supplies bits 8..12 of a block id and which
//!   vanilla never sets, so no vanilla capture can contain one.
//!
//! # What the expected values come from
//!
//! Not from this crate. The canonical block-state ids are looked up **by name**
//! in `lodestone-data`'s jar-derived 26.2 registry, and the legacy
//! metadata→colour correspondence the nibble tests rely on has two independent
//! outside sources that agree: wool placed on a real 1.7.10 server and read
//! back off its wire, and `minecraft-data`'s own 1.7 `blocks.json` variation
//! list. Neither is this decoder.
//!
//! The two detectors that catch what a length check cannot are the same pair
//! the 1.8 era uses: **zero trailing bytes** after decode, and **known blocks
//! at known coordinates**. A transposed or wrongly-grouped decode consumes the
//! byte count perfectly and scrambles positions, and only the second catches
//! it.

use std::io::Write as _;

use lodestone_core::{Reader, Writer};
use lodestone_data::block_states;
use lodestone_v1_7::packets::chunk::{ChunkShape, MapChunk, MapChunkBulk};
use lodestone_world::LightData;

/// Bytes in one section's block-type array: one byte per block, unlike the
/// two bytes per block protocol 47 uses.
const TYPE_BYTES: usize = 4096;
/// Bytes in one nibble array: 4096 nibbles, two per byte.
const NIBBLE_BYTES: usize = 2048;
/// Bytes in the 2-D biome footer.
const BIOME_BYTES: usize = 256;

/// Legacy numeric block ids this era addresses blocks by.
///
/// These are the ids the wire carries, taken from `minecraft-data`'s own 1.7
/// `blocks.json`, and they are what the golden blobs below are *built* from —
/// never what an assertion compares against.
const ID_STONE: u8 = 1;
const ID_BEDROCK: u8 = 7;
const ID_WOOL: u8 = 35;

/// The canonical 26.2 block-state id of a property-less block, looked up **by
/// name** in the jar-derived registry.
///
/// Deliberately does not call the decoder's own canonicalisation to compute
/// what it expects; that would make every assertion below vacuous.
fn canonical_state(name: &str) -> u32 {
    (0..block_states::STATE_COUNT)
        .find(|&id| {
            block_states::block_name(id) == Some(name) && block_states::properties(id) == Some(&[])
        })
        .unwrap_or_else(|| panic!("the 26.2 registry defines a property-less {name}"))
}

/// Section-local flat index, YZX order.
fn idx(x: usize, y: usize, z: usize) -> usize {
    y << 8 | z << 4 | x
}

/// Builds one section's block-type array from a value function.
fn type_bytes(mut id_at: impl FnMut(usize, usize, usize) -> u8) -> Vec<u8> {
    let mut out = vec![0u8; TYPE_BYTES];
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                out[idx(x, y, z)] = id_at(x, y, z);
            }
        }
    }
    out
}

/// Builds one 2048-byte nibble array from a value function.
///
/// Even flat index in the low nibble, odd in the high — the convention
/// measured against a real server and implemented by `NibbleArray`.
fn nibble_bytes(mut value_at: impl FnMut(usize, usize, usize) -> u8) -> Vec<u8> {
    let mut out = vec![0u8; NIBBLE_BYTES];
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                let entry = idx(x, y, z);
                let value = value_at(x, y, z) & 0x0F;
                if entry % 2 == 0 {
                    out[entry / 2] |= value;
                } else {
                    out[entry / 2] |= value << 4;
                }
            }
        }
    }
    out
}

/// One section's worth of golden arrays, in the order the wire groups them.
struct Section {
    types: Vec<u8>,
    metadata: Vec<u8>,
    block_light: Vec<u8>,
    sky_light: Vec<u8>,
    /// High id bits, when this section is named by the second bitmask.
    add: Option<Vec<u8>>,
}

impl Section {
    /// A flat-world-shaped section: bedrock at y 0, stone at y 1, air above.
    fn floor() -> Self {
        Self {
            types: type_bytes(|_, y, _| match y {
                0 => ID_BEDROCK,
                1 => ID_STONE,
                _ => 0,
            }),
            metadata: vec![0u8; NIBBLE_BYTES],
            block_light: vec![0u8; NIBBLE_BYTES],
            sky_light: vec![0xFFu8; NIBBLE_BYTES],
            add: None,
        }
    }
}

/// Assembles a column's arrays in the wire's per-type grouping and returns the
/// uncompressed blob.
///
/// The grouping is the thing measured: every type array first, then every
/// metadata array, and so on across the whole column. Interleaving per section
/// consumes the identical number of bytes and produces garbage, which is why
/// the assertions below are about *positions* rather than lengths.
fn column_blob(sections: &[Section], skylight: bool, biome: Option<u8>) -> Vec<u8> {
    let mut blob = Vec::new();
    for section in sections {
        blob.extend_from_slice(&section.types);
    }
    for section in sections {
        blob.extend_from_slice(&section.metadata);
    }
    for section in sections {
        blob.extend_from_slice(&section.block_light);
    }
    if skylight {
        for section in sections {
            blob.extend_from_slice(&section.sky_light);
        }
    }
    for section in sections {
        if let Some(add) = &section.add {
            blob.extend_from_slice(add);
        }
    }
    if let Some(biome) = biome {
        blob.extend_from_slice(&[biome; BIOME_BYTES]);
    }
    blob
}

/// zlib-deflates a blob the way this era's server does.
fn deflate(blob: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(blob).expect("deflate");
    encoder.finish().expect("finish deflate")
}

/// Frames a `map_chunk` body: two `i32` coordinates, a `bool`, two `u16`
/// bitmasks, an `i32` byte count and the stream.
///
/// Note the `i32` length. Protocol 47 uses a varint here; a varint decoder
/// pointed at these four bytes reads a wildly wrong count.
fn map_chunk_body(x: i32, z: i32, ground_up: bool, primary: u16, add: u16, blob: &[u8]) -> Vec<u8> {
    let compressed = deflate(blob);
    let mut w = Writer::default();
    w.i32(x);
    w.i32(z);
    w.bool(ground_up);
    w.u16(primary);
    w.u16(add);
    w.i32(i32::try_from(compressed.len()).expect("length fits"));
    w.bytes(&compressed);
    w.into_vec()
}

#[test]
fn a_single_column_map_chunk_decodes_with_zero_trailing_bytes() {
    let blob = column_blob(&[Section::floor()], true, Some(1));
    let body = map_chunk_body(3, -5, true, 0x0001, 0x0000, &blob);
    let shape = ChunkShape::overworld();

    let data = MapChunk::decode(&mut Reader::new(&body), &shape).expect("decode map_chunk");

    assert_eq!((data.x, data.z), (3, -5), "the column coordinates decoded");
    assert!(data.ground_up, "the ground-up flag decoded");
    assert!(!data.is_unload(), "a column with a section is not an unload");
    assert_eq!(data.extended_ids, 0, "no add array was sent");
}

#[test]
fn known_blocks_land_at_the_y_layers_they_were_written_at() {
    let blob = column_blob(&[Section::floor()], true, Some(1));
    let body = map_chunk_body(0, 0, true, 0x0001, 0x0000, &blob);

    let data = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect("decode map_chunk");

    let air = canonical_state("minecraft:air");
    let bedrock = canonical_state("minecraft:bedrock");
    let stone = canonical_state("minecraft:stone");
    for (x, z) in [(0, 0), (15, 15), (7, 3), (1, 14)] {
        assert_eq!(data.column.get_block(x, 0, z), bedrock, "bedrock at ({x},0,{z})");
        assert_eq!(data.column.get_block(x, 1, z), stone, "stone at ({x},1,{z})");
        assert_eq!(data.column.get_block(x, 2, z), air, "air at ({x},2,{z})");
    }
}

#[test]
fn metadata_comes_from_its_own_array_and_reads_the_nibble_its_index_names() {
    // Four wool metadata values at adjacent x, chosen so no value equals its
    // byte-partner: under the reversed nibble convention x 0 and 1 would swap,
    // as would 2 and 3, and all four assertions would fail. The expected
    // colours are not derived here — wool placed on a real 1.7.10 server and
    // read back off its wire gave this correspondence, and `minecraft-data`'s
    // own 1.7 variation list agrees with it independently.
    let metas = [(0usize, 14u8, "red"), (1, 1, "orange"), (2, 5, "lime"), (3, 11, "blue")];

    let mut section = Section::floor();
    section.types = type_bytes(|x, y, _| {
        if y == 2 && x < metas.len() {
            ID_WOOL
        } else if y == 0 {
            ID_BEDROCK
        } else {
            0
        }
    });
    section.metadata = nibble_bytes(|x, y, _| {
        if y == 2 {
            metas.get(x).map_or(0, |&(_, meta, _)| meta)
        } else {
            0
        }
    });

    let blob = column_blob(&[section], true, Some(1));
    let body = map_chunk_body(0, 0, true, 0x0001, 0x0000, &blob);
    let data = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect("decode map_chunk");

    for (x, meta, colour) in metas {
        let expected = canonical_state(&format!("minecraft:{colour}_wool"));
        assert_eq!(
            data.column.get_block(x, 2, 0),
            expected,
            "wool metadata {meta} at x {x} should read as {colour} wool"
        );
    }
    // The same wire id with metadata 0 is a different block entirely, which is
    // what proves the metadata array is being read rather than ignored.
    assert_ne!(
        canonical_state("minecraft:white_wool"),
        canonical_state("minecraft:red_wool"),
        "the registry distinguishes the two colours, so the assertions above can fail"
    );
}

/// The exact bytes of one chunk unload, recorded off a real 1.7.10 server.
///
/// Captured by walking a client 320 blocks east on a flat overworld and taking
/// the first `map_chunk` the server sent for a column left behind. Twenty
/// arrived in that walk, byte-identical apart from their coordinates, and not
/// one data-bearing `map_chunk` arrived at all.
///
/// The bytes decode as x 16, z -17, `groundUp` true, both bitmasks zero, and a
/// 12-byte payload — which is the whole point of keeping them: an unload is
/// not an empty body. Those 12 bytes inflate to 256 bytes of biome id 1,
/// because `groundUp` implies a biome footer whether or not a section is
/// present. A decoder that expected nothing here would reject every unload a
/// real server sends.
const RECORDED_UNLOAD: &str = concat!(
    "00000010", // x = 16
    "ffffffef", // z = -17
    "01",       // groundUp
    "0000",     // primary bitmask: no sections
    "0000",     // add bitmask
    "0000000c", // payload length: 12 bytes, not zero
    "789c63641cd9000081800101",
);

fn from_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "a hex body has an even length");
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex digit pair"))
        .collect()
}

#[test]
fn the_recorded_unload_from_a_real_server_decodes_as_an_unload() {
    let body = from_hex(RECORDED_UNLOAD);

    let data = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect("a real server's own unload bytes must decode");

    assert!(data.is_unload(), "an empty ground-up column is an unload");
    assert!(!data.had_sections, "no section was present");
    assert_eq!(
        (data.x, data.z),
        (16, -17),
        "the unload names the column it retires"
    );
}

#[test]
fn an_unload_still_carries_its_biome_footer() {
    // The falsifiable half of the case above: the same framing with a genuinely
    // empty payload is refused, which is what says the 256 bytes are required
    // rather than incidental. Without this, a decoder that ignored the footer
    // would pass the recorded-bytes test just as well.
    let body = map_chunk_body(16, -17, true, 0x0000, 0x0000, &[]);

    let err = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect_err("a ground-up column with no biome footer is not well formed");
    assert!(
        err.to_string().contains("256"),
        "the error should name the 256 bytes it wanted, got: {err}"
    );
}

#[test]
fn a_hand_built_unload_matches_the_recorded_one() {
    // Same coordinates and bitmasks, biome footer built here rather than
    // recorded: the two must agree, which is what lets the rest of this file
    // build bodies by hand and still be talking about the real wire.
    let body = map_chunk_body(16, -17, true, 0x0000, 0x0000, &[1u8; BIOME_BYTES]);

    let mine = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect("decode the hand-built unload");
    let theirs = MapChunk::decode(&mut Reader::new(&from_hex(RECORDED_UNLOAD)), &ChunkShape::overworld())
        .expect("decode the recorded unload");

    assert_eq!(
        (mine.x, mine.z, mine.ground_up, mine.had_sections),
        (theirs.x, theirs.z, theirs.ground_up, theirs.had_sections),
        "the hand-built framing agrees with the recorded one"
    );
}

#[test]
fn a_partial_update_is_not_mistaken_for_an_unload() {
    // The other empty case: not ground-up, no sections. It carries no biome
    // footer and is not an unload, and conflating the two would silently drop
    // a live column.
    // No biome footer, because `groundUp` is false.
    let body = map_chunk_body(4, 4, false, 0x0000, 0x0000, &[]);

    let data = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect("decode partial");

    assert!(!data.ground_up, "the ground-up flag decoded as false");
    assert!(!data.is_unload(), "a non-ground-up empty column is not an unload");
}

#[test]
fn the_add_array_supplies_the_high_bits_of_a_block_id() {
    // Vanilla never sets the second bitmask, so this case cannot come from a
    // capture. The value chosen is id 263 = (1 << 8) | 7, whose low byte is
    // bedrock's id: if the high nibble were dropped the block would decode as
    // bedrock, and if the shift were wrong it would decode as something else
    // again. 263 has no 26.2 counterpart, so the decoder counts it as an
    // extended id and substitutes air rather than inventing a block.
    let mut section = Section::floor();
    section.types = type_bytes(|x, y, z| if (x, y, z) == (0, 5, 0) { ID_BEDROCK } else { 0 });
    section.add = Some(nibble_bytes(|x, y, z| u8::from((x, y, z) == (0, 5, 0))));

    let blob = column_blob(&[section], true, Some(1));
    let body = map_chunk_body(0, 0, true, 0x0001, 0x0001, &blob);
    let data = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect("decode map_chunk with an add array");

    assert_eq!(
        data.extended_ids, 1,
        "exactly one block used the add array's high bits"
    );
    assert_ne!(
        data.column.get_block(0, 5, 0),
        canonical_state("minecraft:bedrock"),
        "id 263 must not decode as the bedrock its low byte names"
    );
    assert!(
        data.fallback.out_of_bounds > 0,
        "an id past 255 is a counted out-of-bounds fallback, not a silent zero"
    );
}

#[test]
fn a_shape_without_skylight_expects_no_sky_arrays() {
    // `map_chunk` cannot tell from its own bytes whether sky light is present,
    // so the caller supplies it. Feeding a nether-shaped column to the
    // overworld shape leaves the geometry 2048 bytes short per section, which
    // the exact-inflated-length check is there to catch.
    let blob = column_blob(&[Section::floor()], false, Some(1));
    let body = map_chunk_body(0, 0, true, 0x0001, 0x0000, &blob);

    let data = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::no_skylight())
        .expect("decode with the matching shape");
    assert!(
        matches!(data.light.sky(1), LightData::Missing),
        "no sky array was sent, so the sky light stays missing"
    );
    assert!(
        matches!(data.light.block(1), LightData::Values(_)),
        "the block light array was still read"
    );

    let err = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect_err("the overworld shape expects 2048 more bytes than were sent");
    let message = err.to_string();
    assert!(
        message.contains("inflated to"),
        "the mismatch should name both lengths, got: {message}"
    );
}

#[test]
fn several_present_sections_decode_at_their_own_indices() {
    // Sections 0 and 4, so a decoder that walked the bitmask as a dense count
    // would put the second one at index 1.
    let mut low = Section::floor();
    low.types = type_bytes(|_, y, _| if y == 0 { ID_BEDROCK } else { 0 });
    let mut high = Section::floor();
    high.types = type_bytes(|_, y, _| if y == 3 { ID_STONE } else { 0 });

    let blob = column_blob(&[low, high], true, Some(1));
    let body = map_chunk_body(0, 0, true, 0x0011, 0x0000, &blob);
    let data = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect("decode two sections");

    assert_eq!(
        data.column.get_block(0, 0, 0),
        canonical_state("minecraft:bedrock"),
        "section 0's bedrock stayed at y 0"
    );
    assert_eq!(
        data.column.get_block(0, 67, 0),
        canonical_state("minecraft:stone"),
        "section 4's stone landed at y 67, not y 19"
    );
    assert_eq!(
        data.column.get_block(0, 19, 0),
        canonical_state("minecraft:air"),
        "nothing was written into the section a dense walk would have used"
    );
}

#[test]
fn the_bulk_packet_reads_its_metadata_after_the_payload() {
    // The field order that separates this era from protocol 47: count, byte
    // count, sky-light flag, payload, and only then the per-column metadata.
    // A single-column packet parses under either order, so this uses two
    // columns with distinct coordinates — under the 1.8 order the first
    // column's x would be read out of the byte count.
    let first = column_blob(&[Section::floor()], true, Some(1));
    let second = column_blob(&[Section::floor()], true, Some(1));
    let mut blob = first;
    blob.extend_from_slice(&second);
    let compressed = deflate(&blob);

    let mut w = Writer::default();
    w.i16(2);
    w.i32(i32::try_from(compressed.len()).expect("length fits"));
    w.bool(true);
    w.bytes(&compressed);
    for (x, z) in [(6i32, -9i32), (7, -9)] {
        w.i32(x);
        w.i32(z);
        w.u16(0x0001);
        w.u16(0x0000);
    }
    let body = w.into_vec();

    let columns = MapChunkBulk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect("decode bulk");

    assert_eq!(columns.len(), 2, "both bundled columns decoded");
    assert_eq!(
        columns.iter().map(|c| (c.x, c.z)).collect::<Vec<_>>(),
        vec![(6, -9), (7, -9)],
        "the trailing metadata matched the payload order"
    );
    assert!(
        columns.iter().all(|c| c.ground_up),
        "every bulk column is a full column"
    );
}

#[test]
fn the_bulk_sky_light_flag_beats_the_callers_shape() {
    // The bulk packet carries its own `skyLightSent`, so a nether bulk packet
    // decodes correctly even when handed the overworld shape. That asymmetry
    // with `map_chunk` is the reason the flag exists.
    let blob = column_blob(&[Section::floor()], false, Some(1));
    let compressed = deflate(&blob);

    let mut w = Writer::default();
    w.i16(1);
    w.i32(i32::try_from(compressed.len()).expect("length fits"));
    w.bool(false);
    w.bytes(&compressed);
    w.i32(0);
    w.i32(0);
    w.u16(0x0001);
    w.u16(0x0000);
    let body = w.into_vec();

    let columns = MapChunkBulk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect("the packet's own flag should win over the shape");

    assert_eq!(columns.len(), 1, "the column decoded");
    assert!(
        matches!(columns[0].light.sky(1), LightData::Missing),
        "no sky array was sent, and the flag said so"
    );
}

#[test]
fn a_payload_that_is_not_a_zlib_stream_is_refused() {
    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.u16(0x0001);
    w.u16(0x0000);
    w.i32(8);
    w.bytes(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x11, 0x22, 0x33]);
    let body = w.into_vec();

    let err = MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect_err("random bytes are not a zlib stream");
    assert!(
        err.to_string().contains("zlib"),
        "the error should say what failed, got: {err}"
    );
}

#[test]
fn a_truncated_body_errors_rather_than_reading_past_the_end() {
    let blob = column_blob(&[Section::floor()], true, Some(1));
    let full = map_chunk_body(0, 0, true, 0x0001, 0x0000, &blob);
    let truncated = &full[..full.len() - 16];

    MapChunk::decode(&mut Reader::new(truncated), &ChunkShape::overworld())
        .expect_err("a body shorter than its own declared length must error");
}

#[test]
fn a_negative_declared_length_is_refused() {
    // The length is an `i32`, so a hostile or corrupt server can make it
    // negative. `usize::try_from` is what stands between that and a panic.
    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.u16(0x0001);
    w.u16(0x0000);
    w.i32(-1);
    let body = w.into_vec();

    MapChunk::decode(&mut Reader::new(&body), &ChunkShape::overworld())
        .expect_err("a negative payload length must not be treated as a huge one");
}
