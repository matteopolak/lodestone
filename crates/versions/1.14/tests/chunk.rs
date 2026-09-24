//! Hermetic 1.16.5 (protocol 754) chunk-decode tests.
//!
//! These build golden `map_chunk` (and `update_light`) blobs and assert the
//! properties that a subtly wrong 1.16 layout would break:
//!
//! * **zero trailing bytes** after decode (`ensure_empty`) — a misparse almost
//!   always leaves the buffer misaligned, so this is the single best detector;
//! * **flat block-state ids land at known Y** — catches a byte-correct but
//!   transposed decode and proves the post-1.13 flattening path (palette entries
//!   are flat state ids, not `(id << 4) | meta`) plus the 1.16 **non-straddling**
//!   long unpacking (a value never spans a 64-bit boundary);
//! * **3-D biomes** (1024 VarInt cells, not a 256-byte 2-D footer) decode into
//!   the version-free biome container;
//! * **light travels separately** — `map_chunk` carries no light, and
//!   `update_light` decodes into a [`LightPatch`];
//! * **truncated / over-long payloads are rejected**, not panicked on — the
//!   negative controls that prove the framing checks actually fire.
//!
//! The section bodies are produced by the world's own
//! [`PalettedContainer::encode`], so the fixtures are exactly what
//! [`PalettedContainer::decode`] round-trips — the test pins the *framing*
//! around the container, which is the version-specific part.

use lodestone_core::{Nbt, Reader, Writer, write_named_nbt};
use lodestone_v1_14::canonical;
use lodestone_v1_14::packets::chunk::{ChunkShape, MapChunk, UpdateLight};
use lodestone_world::{LongArrayFraming, PaletteKind, PalettedContainer, SignText};

// Flat 1.16.5 *wire* block-state ids (post-flattening: no `(blockId << 4) |
// meta`) — 1.16.5's own global-palette numbering, used to build fixtures
// exactly as a real server would encode them. Decoded output no longer
// carries these ids unchanged: `MapChunk::decode` now translates every cell
// through `canonical::resolve_or_air` into the canonical **26.2** id space
// (see `src/canonical.rs`), so assertions on decoded blocks below compare
// against the `_CANONICAL` constants instead.
const AIR_WIRE: u32 = 0;
const BEDROCK_WIRE: u32 = 33; // bedrock's default flat state id in 1.16.5
const STONE_WIRE: u32 = 1;

// The canonical 26.2 ids those wire values decode to. Anchored outside this
// crate's own table: derived independently from the jar-generated
// `tests/support/blocks_1_16_5_jar.json` (1.16.5 side) and
// `.cache/mc/26.2/generated/reports/blocks.json` (26.2 side) — see
// `tests/canonicalisation.rs`'s discriminating-state test for the same
// anchoring method applied to blocks whose ids actually differ. Air and
// stone happen to keep the same low id in both versions (both are among the
// first few blocks ever registered); bedrock does not, which is exactly
// what makes it worth asserting here.
const AIR_CANONICAL: u32 = 0;
const BEDROCK_CANONICAL: u32 = 85;
const STONE_CANONICAL: u32 = 1;

/// A minimal but real named-NBT compound (`TAG_Compound "" { TAG_End }`) — the
/// shape 1.16 uses for the `heightmaps` field. The decoder consumes it.
const HEIGHTMAPS_NBT: [u8; 4] = [0x0A, 0x00, 0x00, 0x00];

/// Container index for section-relative `(x, y, z)` in YZX order.
fn idx(x: usize, y: usize, z: usize) -> usize {
    (y << 8) | (z << 4) | x
}

/// Builds one section body: `[blockCount: i16, PalettedContainer]`, with the
/// container produced by the world encoder so it matches `decode` exactly. No
/// inline light (1.14 moved light to `update_light`).
fn section_body(value_at: impl Fn(usize, usize, usize) -> u32) -> Vec<u8> {
    let kind = PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed);
    let mut values = vec![0u32; 4096];
    let mut non_air = 0i16;
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                let v = value_at(x, y, z);
                values[idx(x, y, z)] = v;
                if v != AIR_WIRE {
                    non_air += 1;
                }
            }
        }
    }
    let container = PalettedContainer::from_values(kind, &values);

    let mut w = Writer::default();
    w.i16(non_air);
    container.encode(&mut w);
    w.into_vec()
}

/// Wraps a single-section `chunkData` blob into a full `map_chunk` packet with
/// 1.16 framing: outer ints, `groundUp`, bitmask, heightmap NBT, 1024 VarInt
/// biomes (full columns only), the length-prefixed `chunkData`, then the
/// block-entity count.
fn build_map_chunk(
    x: i32,
    z: i32,
    section_body: &[u8],
    biome: u32,
    block_entities: i32,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.i32(x);
    w.i32(z);
    w.bool(true); // groundUp (full column)
    w.var_i32(0x0001); // bitmask: section 0 present
    w.bytes(&HEIGHTMAPS_NBT);
    w.var_i32(1024); // biomes array length prefix (1.16.2)
    for _ in 0..1024 {
        w.var_i32(biome as i32); // 3-D biomes: 1024 cells over the column
    }
    w.var_i32(section_body.len() as i32);
    w.bytes(section_body);
    w.var_i32(block_entities);
    w.into_vec()
}

#[test]
fn decodes_full_chunk_zero_trailing_bytes() {
    let section = section_body(|_, y, _| if y == 0 { BEDROCK_WIRE } else { AIR_WIRE });
    let body = build_map_chunk(3, -5, &section, 1, 0);

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld(lodestone_v1_14::PROTOCOL)).expect("decode");
    r.ensure_empty()
        .expect("decode consumes the whole packet (zero trailing bytes)");

    assert_eq!(chunk.x, 3);
    assert_eq!(chunk.z, -5);
    assert!(chunk.ground_up);
    assert_eq!(chunk.column.section_count(), 16);
    // 1.16 light is not in map_chunk.
    assert!(chunk.light.sky(1).get(0).is_none());
}

// ---- Block entities: kept, not discarded, and a sign's text survives. ----

/// Finds a wire block-state id in this protocol's own global palette that
/// canonicalises to one of the sign block types. Derived from the same
/// generated wire -> canonical table [`MapChunk::decode`] itself calls
/// through `ChunkShape::canonical`, which is an outside source relative to
/// the block-entity translation this test exercises: that translation reads
/// the block state already resolved at the entity's position, it does not
/// consult this table (or this crate's own `id`-string field) at all.
fn find_wire_sign_id(table: &canonical::CanonicalTable) -> u32 {
    (0..table.state_count())
        .find(|&wire| {
            table
                .resolve(wire)
                .map(|state_id| state_id.name())
                .is_some_and(|name| name.ends_with("_sign"))
        })
        .expect("1.16.5's table has at least one wire state that canonicalises to a sign")
}

/// A real 1.16.5-shaped sign block entity: `x`/`y`/`z` and `id` embedded in
/// the compound itself (no wire header), `Text1` a JSON chat-component
/// string — the pre-1.20 wire form every legacy protocol here uses for chat
/// and sign text alike.
fn sign_block_entity_nbt(x: i32, y: i32, z: i32) -> Nbt {
    Nbt::Compound(vec![
        ("id".to_owned(), Nbt::String("minecraft:sign".to_owned())),
        ("x".to_owned(), Nbt::Int(x)),
        ("y".to_owned(), Nbt::Int(y)),
        ("z".to_owned(), Nbt::Int(z)),
        (
            "Text1".to_owned(),
            Nbt::String("{\"text\":\"LODESTONE PROBE 42\"}".to_owned()),
        ),
    ])
}

#[test]
fn sign_text_survives_the_legacy_block_entity_list() {
    let table = canonical::table_for(lodestone_v1_14::PROTOCOL);
    let sign_wire_id = find_wire_sign_id(table);

    // A sign at world (5, 3, 8) inside chunk (0, 0) — section-relative
    // (5, 3, 8) — clear of the bedrock floor this fixture also carries at
    // y=0, so the two block-state writes cannot be confused for each other.
    let section = section_body(|x, y, z| match (x, y, z) {
        (5, 3, 8) => sign_wire_id,
        (_, 0, _) => BEDROCK_WIRE,
        _ => AIR_WIRE,
    });
    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true); // groundUp (full column)
    w.var_i32(0x0001); // bitmask: section 0 present
    w.bytes(&HEIGHTMAPS_NBT);
    w.var_i32(1024);
    for _ in 0..1024 {
        w.var_i32(1); // biome id, irrelevant here
    }
    w.var_i32(section.len() as i32);
    w.bytes(&section);
    w.var_i32(1); // one block entity
    write_named_nbt(&mut w, "", &sign_block_entity_nbt(5, 3, 8)).expect("encode sign nbt");
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld(lodestone_v1_14::PROTOCOL))
        .expect("decode");
    r.ensure_empty()
        .expect("the block-entity list must still leave zero trailing bytes");

    assert_eq!(
        chunk.block_entities.len(),
        1,
        "the sign must survive decode as a real BlockEntity, not be thrown away"
    );
    let entity = &chunk.block_entities[0];
    assert_eq!(entity.rel_x, 5);
    assert_eq!(entity.rel_z, 8);
    assert_eq!(entity.y, 3);

    let text = SignText::parse(&entity.nbt);
    let line: String = text.front.lines[0]
        .iter()
        .map(|span| span.text.as_str())
        .collect();
    assert_eq!(
        line, "LODESTONE PROBE 42",
        "a sign with known text must decode to that exact text, not an empty or default line"
    );
}

#[test]
fn flattening_and_non_straddling_land_flat_ids_at_known_y() {
    // Bedrock at y=0, stone at y=1, air above — flat state ids, non-straddling.
    let section = section_body(|_, y, _| match y {
        0 => BEDROCK_WIRE,
        1 => STONE_WIRE,
        _ => AIR_WIRE,
    });
    let body = build_map_chunk(0, 0, &section, 1, 0);

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld(lodestone_v1_14::PROTOCOL)).expect("decode");
    r.ensure_empty().expect("aligned");

    let col = &chunk.column;
    for x in [0usize, 7, 15] {
        for z in [0usize, 9, 15] {
            assert_eq!(col.get_block(x, 0, z), BEDROCK_CANONICAL, "bedrock at y=0 ({x},{z})");
            assert_eq!(col.get_block(x, 1, z), STONE_CANONICAL, "stone at y=1 ({x},{z})");
            assert_eq!(col.get_block(x, 2, z), AIR_CANONICAL, "air at y=2 ({x},{z})");
        }
    }
}

#[test]
fn three_dimensional_biomes_decode() {
    // biome id 4 in every one of the 1024 cells; the shape default is 0, so a
    // no-op decode would read 0 here.
    let section = section_body(|_, y, _| if y == 0 { BEDROCK_WIRE } else { AIR_WIRE });
    let body = build_map_chunk(0, 0, &section, 4, 0);

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld(lodestone_v1_14::PROTOCOL)).expect("decode");
    r.ensure_empty().expect("aligned");

    // Biome cells are 4×4×4 per section; x/z in 0..4, y in the section's range.
    assert_eq!(chunk.column.get_biome(0, 0, 0), 4);
    assert_eq!(chunk.column.get_biome(3, 5, 3), 4);
}

#[test]
fn partial_update_carries_no_biomes() {
    // A non-full ("groundUp = false") column omits the 1024-cell biome array.
    let section = section_body(|_, y, _| if y == 0 { BEDROCK_WIRE } else { AIR_WIRE });
    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(false); // partial update
    w.var_i32(0x0001);
    w.bytes(&HEIGHTMAPS_NBT);
    // no biome array
    w.var_i32(section.len() as i32);
    w.bytes(&section);
    w.var_i32(0);
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld(lodestone_v1_14::PROTOCOL)).expect("decode");
    r.ensure_empty()
        .expect("partial-update geometry consumes the whole packet");

    assert!(!chunk.ground_up);
    assert_eq!(chunk.column.get_block(0, 0, 0), BEDROCK_CANONICAL);
}

// ---- update_light: light left map_chunk in 1.14. ----

/// Builds an `update_light` packet with the 1.16 single-VarInt masks: one full
/// sky array and one full block array, both at light-section index 1.
fn build_update_light(x: i32, z: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(x);
    w.var_i32(z);
    w.bool(true); // trustEdges
    w.var_i32(0x0002); // sky mask: section 1
    w.var_i32(0x0002); // block mask: section 1
    w.var_i32(0x0000); // empty sky mask
    w.var_i32(0x0000); // empty block mask
    w.var_i32(2048); // sky array length
    w.bytes(&[0xFFu8; 2048]);
    w.var_i32(2048); // block array length
    w.bytes(&[0x00u8; 2048]);
    w.into_vec()
}

#[test]
fn update_light_decodes_masks_and_arrays() {
    let body = build_update_light(3, -5);
    let mut r = Reader::new(&body);
    let update = UpdateLight::decode(&mut r, lodestone_v1_14::PROTOCOL).expect("decode");
    r.ensure_empty().expect("update_light fully consumed");

    assert_eq!(update.x, 3);
    assert_eq!(update.z, -5);
    // One sky section + one block section named by the masks.
    assert_eq!(update.patch.len(), 2);
    assert!(!update.patch.is_empty());
}

// ---- Negative controls: the checks guarding the absence claims must fire. ----

#[test]
fn truncated_blob_errors_cleanly() {
    // A declared chunkData length longer than the bytes present must error.
    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.var_i32(0x0001);
    w.bytes(&HEIGHTMAPS_NBT);
    w.var_i32(1024);
    for _ in 0..1024 {
        w.var_i32(1);
    }
    w.var_i32(10_000); // claims 10000 bytes
    w.bytes(&[0u8; 100]); // but only 100 present
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let result = MapChunk::decode(&mut r, &ChunkShape::overworld(lodestone_v1_14::PROTOCOL));
    assert!(result.is_err(), "truncated blob must error, not panic");
}

#[test]
fn extra_trailing_bytes_rejected() {
    // A valid section padded with stray bytes decodes the geometry but must fail
    // the zero-trailing-bytes detector inside the bounded chunkData sub-reader.
    let section = section_body(|_, y, _| if y == 0 { BEDROCK_WIRE } else { AIR_WIRE });
    let mut blob = section;
    blob.extend_from_slice(&[0u8; 8]); // 8 stray trailing bytes

    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true);
    w.var_i32(0x0001);
    w.bytes(&HEIGHTMAPS_NBT);
    w.var_i32(1024);
    for _ in 0..1024 {
        w.var_i32(1);
    }
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);
    w.var_i32(0);
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let result = MapChunk::decode(&mut r, &ChunkShape::overworld(lodestone_v1_14::PROTOCOL));
    assert!(result.is_err(), "extra blob bytes must be rejected");
}

#[test]
fn update_light_wrong_array_length_rejected() {
    // A light array whose declared length is not 2048 bytes must be rejected.
    let mut w = Writer::default();
    w.var_i32(0);
    w.var_i32(0);
    w.bool(true);
    w.var_i32(0x0002); // sky mask: section 1
    w.var_i32(0x0000);
    w.var_i32(0x0000);
    w.var_i32(0x0000);
    w.var_i32(1024); // WRONG: not 2048
    w.bytes(&[0xFFu8; 1024]);
    let body = w.into_vec();

    let mut r = Reader::new(&body);
    let result = UpdateLight::decode(&mut r, lodestone_v1_14::PROTOCOL);
    assert!(result.is_err(), "wrong light-array length must be rejected");
}
