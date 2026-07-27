use lodestone_core::{Bound, Ctx, Decode, Encode, Error, Packet, Reader, State, Writer};
use lodestone_macros::{Decode, Encode, Packet};

fn encode_to_vec<T: Encode>(value: &T, version: i32) -> lodestone_core::Result<Vec<u8>> {
    let mut writer = Writer::default();
    value.encode(&mut writer, Ctx { version })?;
    Ok(writer.into_vec())
}

fn decode_from_slice<T: Decode>(bytes: &[u8], version: i32) -> lodestone_core::Result<T> {
    let mut reader = Reader::new(bytes);
    let value = T::decode(&mut reader, Ctx { version })?;
    reader.ensure_empty()?;
    Ok(value)
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct NamedPacket {
    id: u8,
    #[mc(varint)]
    count: i32,
    #[mc(max = 8)]
    name: String,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct TuplePacket(#[mc(varlong)] i64, bool);

#[derive(Debug, PartialEq, Encode, Decode)]
struct UnitPacket;

#[derive(Debug, PartialEq, Encode, Decode)]
struct VersionedPacket {
    base: u8,
    #[mc(since = 107)]
    modern: u8,
    #[mc(until = 340)]
    legacy: u8,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct LimitedVec {
    #[mc(len = "u8", max = 2)]
    values: Vec<u8>,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct VarIntVecPacket {
    #[mc(varint)]
    ids: Vec<i32>,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct ConditionalTailPacket {
    item_id: i16,
    #[mc(present_if = "item_id != -1")]
    count: i8,
    #[mc(present_if = "item_id != -1")]
    damage: i16,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct ConditionalOptionTailPacket {
    window_id: u8,
    #[mc(max = 32)]
    inventory_type: String,
    slot_count: u8,
    #[mc(when = "inventory_type == \"EntityHorse\"")]
    entity_id: Option<i32>,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct ShadowedDecodeNamesPacket {
    r: u8,
    ctx: u8,
    tail: u8,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct RemainingPacket {
    prefix: u8,
    #[mc(remaining)]
    rest: Vec<u8>,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct SkippedPacket {
    first: u8,
    #[mc(skip)]
    ignored: u8,
    last: u8,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct FixedArrayPacket {
    prefix: u8,
    #[mc(fixed = 3)]
    last_seen: [u8; 3],
    suffix: u8,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct FixedVecPacket {
    prefix: u8,
    #[mc(fixed = 3)]
    payload: Vec<u8>,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct VersionedFixedPacket {
    prefix: u8,
    #[mc(fixed = 3, since = 107, until = 340)]
    gated: [u8; 3],
    suffix: u8,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct FixedUuidSizedPacket {
    #[mc(fixed = 16)]
    bytes: [u8; 16],
}

#[derive(Debug, PartialEq, Encode, Decode)]
#[repr(u8)]
#[mc(repr = "u8")]
enum ExampleEnum {
    Ping = 1,
    Pong(u8) = 2,
    Named { value: u8 } = 3,
}

#[derive(Debug, PartialEq, Encode, Decode)]
enum VarIntEnum {
    Large = 300,
}

#[derive(Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Server)]
struct KeepAlive;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaletteKind {
    Blocks,
    Biomes,
}

#[derive(Clone, Copy, Debug)]
struct ChunkLikeShape {
    palette_kind: PaletteKind,
    heightmap_bytes: usize,
}

#[derive(Debug, PartialEq)]
struct PalettePayload {
    kind: PaletteKind,
    bytes: Vec<u8>,
}

#[derive(Debug, PartialEq)]
struct HeightmapPayload(Vec<u8>);

fn decode_palette_payload(
    r: &mut Reader<'_>,
    _ctx: Ctx,
    shape: &ChunkLikeShape,
) -> lodestone_core::Result<PalettePayload> {
    let len = match shape.palette_kind {
        PaletteKind::Blocks => 3,
        PaletteKind::Biomes => 1,
    };
    Ok(PalettePayload {
        kind: shape.palette_kind,
        bytes: r.bytes(len)?.to_vec(),
    })
}

fn decode_heightmap_payload(
    r: &mut Reader<'_>,
    _ctx: Ctx,
    shape: &ChunkLikeShape,
) -> lodestone_core::Result<HeightmapPayload> {
    Ok(HeightmapPayload(r.bytes(shape.heightmap_bytes)?.to_vec()))
}

#[derive(Debug, PartialEq, Decode)]
#[mc(decode_context = "ChunkLikeShape")]
struct ContextualChunkLikePacket {
    prefix: u8,
    #[mc(decode_with = "decode_palette_payload")]
    palette: PalettePayload,
    #[mc(decode_with = "decode_heightmap_payload")]
    heightmaps: HeightmapPayload,
    suffix: u8,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct PackedPositionV47 {
    #[mc(bits = 26, signed)]
    x: i32,
    #[mc(bits = 12, signed)]
    y: i32,
    #[mc(bits = 26, signed)]
    z: i32,
}

#[derive(Debug, PartialEq, Encode, Decode)]
struct PackedPositionModern {
    #[mc(bits = 26, signed)]
    x: i32,
    #[mc(bits = 26, signed)]
    z: i32,
    #[mc(bits = 12, signed)]
    y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BlockPos {
    x: i32,
    y: i32,
    z: i32,
}

#[derive(Debug, PartialEq, Encode, Decode)]
#[mc(bits(x = 26, y = 12, z = 26, order = "xyz"))]
struct BlockPositionV47(BlockPos);

#[derive(Debug, PartialEq, Encode, Decode)]
#[mc(bits(x = 26, z = 26, y = 12, order = "xzy"))]
struct BlockPositionModern(BlockPos);

// Keep this tiny alternate target only to prove #[mc(crate_path = "...")]
// retargets generated impl paths. All byte-level derive tests above use
// lodestone_core directly so Reader/Writer API drift is caught immediately.
mod alternate_core {
    #[derive(Clone, Copy, Debug)]
    pub struct Ctx {
        pub _version: i32,
    }

    #[derive(Debug)]
    pub struct Error;

    pub type Result<T> = core::result::Result<T, Error>;

    #[derive(Debug)]
    pub struct Reader<'a> {
        _bytes: &'a [u8],
    }

    impl<'a> Reader<'a> {
        pub const fn new(bytes: &'a [u8]) -> Self {
            Self { _bytes: bytes }
        }
    }

    #[derive(Debug, Default)]
    pub struct Writer;

    pub trait Encode {
        fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()>;
    }

    pub trait Decode: Sized {
        fn decode(r: &mut Reader<'_>, ctx: Ctx) -> Result<Self>;
    }

    pub trait Packet {
        const NAME: &'static str;
        const STATE: State;
        const BOUND: Bound;
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum State {
        Status,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Bound {
        Client,
    }
}

#[derive(Debug, PartialEq, Encode, Decode, Packet)]
#[mc(crate_path = "crate::alternate_core", name = "minecraft:retarget", state = Status, bound = Client)]
struct RetargetedUnit;

#[test]
fn named_struct_round_trips_against_core_with_golden_bytes() {
    let value = NamedPacket {
        id: 7,
        count: 300,
        name: "stone".to_owned(),
    };
    let bytes = encode_to_vec(&value, 47).unwrap();
    assert_eq!(bytes, vec![7, 0xac, 0x02, 5, b's', b't', b'o', b'n', b'e']);
    assert_eq!(decode_from_slice::<NamedPacket>(&bytes, 47).unwrap(), value);
}

#[test]
fn tuple_struct_round_trips_and_varlong_is_compact() {
    let value = TuplePacket(300, true);
    let bytes = encode_to_vec(&value, 47).unwrap();
    assert_eq!(bytes, vec![0xac, 0x02, 1]);
    assert_eq!(decode_from_slice::<TuplePacket>(&bytes, 47).unwrap(), value);
}

#[test]
fn unit_struct_round_trips_to_empty_bytes() {
    let value = UnitPacket;
    let bytes = encode_to_vec(&value, 47).unwrap();
    assert!(bytes.is_empty());
    assert_eq!(decode_from_slice::<UnitPacket>(&bytes, 47).unwrap(), value);
}

#[test]
fn since_until_change_wire_bytes_and_round_trip_by_version() {
    let value = VersionedPacket {
        base: 9,
        modern: 10,
        legacy: 11,
    };

    let v47 = encode_to_vec(&value, 47).unwrap();
    assert_eq!(v47, vec![9, 11]);
    assert_eq!(
        decode_from_slice::<VersionedPacket>(&v47, 47).unwrap(),
        VersionedPacket {
            base: 9,
            modern: 0,
            legacy: 11,
        }
    );

    let v340 = encode_to_vec(&value, 340).unwrap();
    assert_eq!(v340, vec![9, 10, 11]);
    assert_eq!(
        decode_from_slice::<VersionedPacket>(&v340, 340).unwrap(),
        value
    );

    let v776 = encode_to_vec(&value, 776).unwrap();
    assert_eq!(v776, vec![9, 10]);
    assert_eq!(
        decode_from_slice::<VersionedPacket>(&v776, 776).unwrap(),
        VersionedPacket {
            base: 9,
            modern: 10,
            legacy: 0,
        }
    );
}

#[test]
fn enum_discriminants_encode_decode_and_unknown_errors() {
    assert_eq!(encode_to_vec(&ExampleEnum::Ping, 47).unwrap(), vec![1]);
    assert_eq!(
        encode_to_vec(&ExampleEnum::Pong(5), 47).unwrap(),
        vec![2, 5]
    );
    assert_eq!(
        encode_to_vec(&ExampleEnum::Named { value: 6 }, 47).unwrap(),
        vec![3, 6]
    );
    assert_eq!(
        decode_from_slice::<ExampleEnum>(&[1], 47).unwrap(),
        ExampleEnum::Ping
    );
    assert_eq!(
        decode_from_slice::<ExampleEnum>(&[2, 5], 47).unwrap(),
        ExampleEnum::Pong(5)
    );
    assert_eq!(
        decode_from_slice::<ExampleEnum>(&[3, 6], 47).unwrap(),
        ExampleEnum::Named { value: 6 }
    );
    assert_eq!(
        decode_from_slice::<ExampleEnum>(&[99], 47),
        Err(Error::InvalidEnumVariant {
            name: "ExampleEnum",
            value: 99,
        })
    );
}

#[test]
fn varint_enum_repr_is_default() {
    let bytes = encode_to_vec(&VarIntEnum::Large, 47).unwrap();
    assert_eq!(bytes, vec![0xac, 0x02]);
    assert_eq!(
        decode_from_slice::<VarIntEnum>(&bytes, 47).unwrap(),
        VarIntEnum::Large
    );
}

#[test]
fn varint_vec_encodes_length_prefixed_varint_elements() {
    let value = VarIntVecPacket {
        ids: vec![1, 300, -1],
    };
    let bytes = encode_to_vec(&value, 47).unwrap();
    assert_eq!(
        bytes,
        vec![0x03, 0x01, 0xac, 0x02, 0xff, 0xff, 0xff, 0xff, 0x0f]
    );
    assert_eq!(
        decode_from_slice::<VarIntVecPacket>(&bytes, 47).unwrap(),
        value
    );
}

#[test]
fn present_if_skips_conditional_tail_when_predicate_is_false() {
    let empty = ConditionalTailPacket {
        item_id: -1,
        count: 64,
        damage: 5,
    };
    let bytes = encode_to_vec(&empty, 47).unwrap();
    assert_eq!(bytes, vec![0xff, 0xff]);
    assert_eq!(
        decode_from_slice::<ConditionalTailPacket>(&bytes, 47).unwrap(),
        ConditionalTailPacket {
            item_id: -1,
            count: 0,
            damage: 0,
        }
    );

    let present = ConditionalTailPacket {
        item_id: 5,
        count: 64,
        damage: 12,
    };
    let bytes = encode_to_vec(&present, 47).unwrap();
    assert_eq!(bytes, vec![0x00, 0x05, 64, 0x00, 0x0c]);
    assert_eq!(
        decode_from_slice::<ConditionalTailPacket>(&bytes, 47).unwrap(),
        present
    );
}

#[test]
fn when_decodes_conditional_option_without_option_presence_byte() {
    let absent = ConditionalOptionTailPacket {
        window_id: 1,
        inventory_type: "minecraft:chest".to_owned(),
        slot_count: 27,
        entity_id: Some(99),
    };
    let bytes = encode_to_vec(&absent, 47).unwrap();
    assert_eq!(
        bytes,
        vec![
            1, 15, b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b'c', b'h',
            b'e', b's', b't', 27
        ]
    );
    assert_eq!(
        decode_from_slice::<ConditionalOptionTailPacket>(&bytes, 47).unwrap(),
        ConditionalOptionTailPacket {
            window_id: 1,
            inventory_type: "minecraft:chest".to_owned(),
            slot_count: 27,
            entity_id: None,
        }
    );

    let present = ConditionalOptionTailPacket {
        window_id: 2,
        inventory_type: "EntityHorse".to_owned(),
        slot_count: 2,
        entity_id: Some(12345),
    };
    let bytes = encode_to_vec(&present, 47).unwrap();
    assert_eq!(
        bytes,
        vec![
            2, 11, b'E', b'n', b't', b'i', b't', b'y', b'H', b'o', b'r', b's', b'e', 2, 0, 0,
            0x30, 0x39
        ]
    );
    assert_eq!(
        decode_from_slice::<ConditionalOptionTailPacket>(&bytes, 47).unwrap(),
        present
    );
}

#[test]
fn when_errors_when_required_option_value_is_missing_on_encode() {
    let value = ConditionalOptionTailPacket {
        window_id: 2,
        inventory_type: "EntityHorse".to_owned(),
        slot_count: 2,
        entity_id: None,
    };

    assert_eq!(
        encode_to_vec(&value, 47),
        Err(Error::Custom(
            "conditional field entity_id is required when its predicate is true".to_owned()
        ))
    );
}

#[test]
fn decode_field_names_do_not_shadow_reader_or_context_bindings() {
    let value = ShadowedDecodeNamesPacket {
        r: 1,
        ctx: 2,
        tail: 3,
    };
    let bytes = encode_to_vec(&value, 47).unwrap();
    assert_eq!(bytes, vec![1, 2, 3]);
    assert_eq!(
        decode_from_slice::<ShadowedDecodeNamesPacket>(&bytes, 47).unwrap(),
        value
    );
}

#[test]
fn max_limits_are_enforced_for_encode_and_decode() {
    let too_many = LimitedVec {
        values: vec![1, 2, 3],
    };
    assert_eq!(
        encode_to_vec(&too_many, 47),
        Err(Error::LimitExceeded {
            limit: 2,
            actual: 3,
        })
    );
    assert_eq!(
        decode_from_slice::<LimitedVec>(&[3, 1, 2, 3], 47),
        Err(Error::LimitExceeded {
            limit: 2,
            actual: 3,
        })
    );

    let too_long = NamedPacket {
        id: 1,
        count: 1,
        name: "too-long!".to_owned(),
    };
    assert_eq!(
        encode_to_vec(&too_long, 47),
        Err(Error::LimitExceeded {
            limit: 8,
            actual: 9,
        })
    );
}

#[test]
fn remaining_consumes_all_trailing_bytes() {
    let decoded = decode_from_slice::<RemainingPacket>(&[4, 1, 2, 3], 47).unwrap();
    assert_eq!(
        decoded,
        RemainingPacket {
            prefix: 4,
            rest: vec![1, 2, 3],
        }
    );
    assert_eq!(encode_to_vec(&decoded, 47).unwrap(), vec![4, 1, 2, 3]);
}

#[test]
fn skip_omits_field_and_decodes_default() {
    let value = SkippedPacket {
        first: 1,
        ignored: 99,
        last: 2,
    };
    let bytes = encode_to_vec(&value, 47).unwrap();
    assert_eq!(bytes, vec![1, 2]);
    assert_eq!(
        decode_from_slice::<SkippedPacket>(&bytes, 47).unwrap(),
        SkippedPacket {
            first: 1,
            ignored: 0,
            last: 2,
        }
    );
}

#[test]
fn fixed_array_round_trips_with_no_length_prefix() {
    let value = FixedArrayPacket {
        prefix: 9,
        last_seen: [1, 2, 3],
        suffix: 8,
    };

    let bytes = encode_to_vec(&value, 776).unwrap();
    assert_eq!(bytes, vec![9, 1, 2, 3, 8]);
    assert_eq!(
        decode_from_slice::<FixedArrayPacket>(&bytes, 776).unwrap(),
        value
    );
}

#[test]
fn fixed_vec_round_trips_with_declared_length() {
    let value = FixedVecPacket {
        prefix: 7,
        payload: vec![4, 5, 6],
    };

    let bytes = encode_to_vec(&value, 776).unwrap();
    assert_eq!(bytes, vec![7, 4, 5, 6]);
    assert_eq!(
        decode_from_slice::<FixedVecPacket>(&bytes, 776).unwrap(),
        value
    );
}

#[test]
fn fixed_vec_encode_enforces_declared_length() {
    let value = FixedVecPacket {
        prefix: 7,
        payload: vec![4, 5],
    };

    assert_eq!(
        encode_to_vec(&value, 776),
        Err(Error::LimitExceeded {
            limit: 3,
            actual: 2,
        })
    );
}

#[test]
fn fixed_decode_errors_on_truncated_input() {
    assert_eq!(
        decode_from_slice::<FixedArrayPacket>(&[9, 1, 2], 776),
        Err(Error::UnexpectedEof)
    );
}

#[test]
fn fixed_respects_since_until_predicates() {
    let value = VersionedFixedPacket {
        prefix: 9,
        gated: [1, 2, 3],
        suffix: 8,
    };

    let v47 = encode_to_vec(&value, 47).unwrap();
    assert_eq!(v47, vec![9, 8]);
    assert_eq!(
        decode_from_slice::<VersionedFixedPacket>(&v47, 47).unwrap(),
        VersionedFixedPacket {
            prefix: 9,
            gated: [0, 0, 0],
            suffix: 8,
        }
    );

    let v340 = encode_to_vec(&value, 340).unwrap();
    assert_eq!(v340, vec![9, 1, 2, 3, 8]);
    assert_eq!(
        decode_from_slice::<VersionedFixedPacket>(&v340, 340).unwrap(),
        value
    );

    let v776 = encode_to_vec(&value, 776).unwrap();
    assert_eq!(v776, vec![9, 8]);
    assert_eq!(
        decode_from_slice::<VersionedFixedPacket>(&v776, 776).unwrap(),
        VersionedFixedPacket {
            prefix: 9,
            gated: [0, 0, 0],
            suffix: 8,
        }
    );
}

#[test]
fn fixed_on_sixteen_byte_array_writes_raw_bytes_instead_of_uuid() {
    let value = FixedUuidSizedPacket {
        bytes: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    };

    let bytes = encode_to_vec(&value, 776).unwrap();
    assert_eq!(bytes, value.bytes);
    assert_eq!(
        decode_from_slice::<FixedUuidSizedPacket>(&bytes, 776).unwrap(),
        value
    );
}

#[test]
fn packet_derive_exposes_metadata() {
    assert_eq!(<KeepAlive as Packet>::NAME, "minecraft:keep_alive");
    assert_eq!(<KeepAlive as Packet>::STATE, State::Play);
    assert_eq!(<KeepAlive as Packet>::BOUND, Bound::Server);
}

#[test]
fn decode_context_threads_runtime_shape_to_custom_field_decoders() {
    let block_shape = ChunkLikeShape {
        palette_kind: PaletteKind::Blocks,
        heightmap_bytes: 2,
    };
    let biome_shape = ChunkLikeShape {
        palette_kind: PaletteKind::Biomes,
        heightmap_bytes: 4,
    };

    let mut block_reader = Reader::new(&[9, 1, 2, 3, 4, 5, 8]);
    let block = ContextualChunkLikePacket::decode_with(
        &mut block_reader,
        Ctx { version: 776 },
        &block_shape,
    )
    .unwrap();
    block_reader.ensure_empty().unwrap();
    assert_eq!(
        block,
        ContextualChunkLikePacket {
            prefix: 9,
            palette: PalettePayload {
                kind: PaletteKind::Blocks,
                bytes: vec![1, 2, 3],
            },
            heightmaps: HeightmapPayload(vec![4, 5]),
            suffix: 8,
        }
    );

    let mut biome_reader = Reader::new(&[9, 7, 1, 2, 3, 4, 8]);
    let biome = ContextualChunkLikePacket::decode_with(
        &mut biome_reader,
        Ctx { version: 776 },
        &biome_shape,
    )
    .unwrap();
    biome_reader.ensure_empty().unwrap();
    assert_eq!(
        biome,
        ContextualChunkLikePacket {
            prefix: 9,
            palette: PalettePayload {
                kind: PaletteKind::Biomes,
                bytes: vec![7],
            },
            heightmaps: HeightmapPayload(vec![1, 2, 3, 4]),
            suffix: 8,
        }
    );
}

#[test]
fn bitpacked_v47_position_uses_x_y_z_order_and_golden_bytes() {
    let value = PackedPositionV47 { x: 1, y: 2, z: 3 };
    let bytes = encode_to_vec(&value, 47).unwrap();
    assert_eq!(bytes, 0x0000_0040_0800_0003_i64.to_be_bytes());
    assert_eq!(
        decode_from_slice::<PackedPositionV47>(&bytes, 47).unwrap(),
        value
    );
}

#[test]
fn bitpacked_modern_position_uses_x_z_y_order_and_golden_bytes() {
    let value = PackedPositionModern { x: 1, z: 3, y: 2 };
    let bytes = encode_to_vec(&value, 776).unwrap();
    assert_eq!(bytes, 0x0000_0040_0000_3002_i64.to_be_bytes());
    assert_eq!(
        decode_from_slice::<PackedPositionModern>(&bytes, 776).unwrap(),
        value
    );
}

#[test]
fn block_position_v47_container_bits_use_x_y_z_order_and_golden_bytes() {
    let value = BlockPositionV47(BlockPos { x: 1, y: 2, z: 3 });
    let bytes = encode_to_vec(&value, 47).unwrap();
    assert_eq!(bytes, 0x0000_0040_0800_0003_i64.to_be_bytes());
    assert_eq!(
        decode_from_slice::<BlockPositionV47>(&bytes, 47).unwrap(),
        value
    );
}

#[test]
fn block_position_modern_container_bits_use_x_z_y_order_and_golden_bytes() {
    let value = BlockPositionModern(BlockPos { x: 1, y: 2, z: 3 });
    let bytes = encode_to_vec(&value, 776).unwrap();
    assert_eq!(bytes, 0x0000_0040_0000_3002_i64.to_be_bytes());
    assert_eq!(
        decode_from_slice::<BlockPositionModern>(&bytes, 776).unwrap(),
        value
    );
}

#[test]
fn block_position_container_bits_sign_extend_all_ones_to_negative_one() {
    let all_ones = (-1_i64).to_be_bytes();
    assert_eq!(
        decode_from_slice::<BlockPositionV47>(&all_ones, 47).unwrap(),
        BlockPositionV47(BlockPos {
            x: -1,
            y: -1,
            z: -1
        })
    );
    assert_eq!(
        decode_from_slice::<BlockPositionModern>(&all_ones, 776).unwrap(),
        BlockPositionModern(BlockPos {
            x: -1,
            y: -1,
            z: -1
        })
    );
}

#[test]
fn block_position_container_bits_round_trip_negative_values_and_extremes() {
    let cases = [
        BlockPos { x: 0, y: 0, z: 0 },
        BlockPos {
            x: -1,
            y: -1,
            z: -1,
        },
        BlockPos {
            x: -33_554_432,
            y: -2048,
            z: -33_554_432,
        },
        BlockPos {
            x: 33_554_431,
            y: 2047,
            z: 33_554_431,
        },
        BlockPos {
            x: -30_000_000,
            y: 80,
            z: 30_000_000,
        },
    ];

    for case in cases {
        let v47 = BlockPositionV47(case);
        let v47_bytes = encode_to_vec(&v47, 47).unwrap();
        assert_eq!(
            decode_from_slice::<BlockPositionV47>(&v47_bytes, 47).unwrap(),
            v47
        );

        let modern = BlockPositionModern(case);
        let modern_bytes = encode_to_vec(&modern, 776).unwrap();
        assert_eq!(
            decode_from_slice::<BlockPositionModern>(&modern_bytes, 776).unwrap(),
            modern
        );
    }
}

#[test]
fn bitpacked_signed_fields_round_trip_negative_values_and_extremes() {
    let cases = [
        PackedPositionV47 { x: 0, y: 0, z: 0 },
        PackedPositionV47 {
            x: -1,
            y: -1,
            z: -1,
        },
        PackedPositionV47 {
            x: -33_554_432,
            y: -2048,
            z: -33_554_432,
        },
        PackedPositionV47 {
            x: 33_554_431,
            y: 2047,
            z: 33_554_431,
        },
        PackedPositionV47 {
            x: -1,
            y: 64,
            z: 30_000_000,
        },
    ];

    for case in cases {
        let bytes = encode_to_vec(&case, 47).unwrap();
        assert_eq!(
            decode_from_slice::<PackedPositionV47>(&bytes, 47).unwrap(),
            case
        );
    }

    assert_eq!(
        encode_to_vec(
            &PackedPositionV47 {
                x: -1,
                y: -1,
                z: -1
            },
            47
        )
        .unwrap(),
        (-1_i64).to_be_bytes()
    );
}

#[test]
fn bitpacked_encode_rejects_values_outside_the_declared_signed_width() {
    assert_eq!(
        encode_to_vec(
            &PackedPositionV47 {
                x: 33_554_432,
                y: 0,
                z: 0,
            },
            47,
        ),
        Err(Error::Custom(
            "bit field x=33554432 does not fit signed 26-bit range -33554432..=33554431".to_owned()
        ))
    );
}

#[test]
fn crate_path_can_retarget_generated_impls() {
    let mut writer = alternate_core::Writer;
    let ctx = alternate_core::Ctx { _version: 776 };
    alternate_core::Encode::encode(&RetargetedUnit, &mut writer, ctx).unwrap();

    let mut reader = alternate_core::Reader::new(&[]);
    assert_eq!(
        <RetargetedUnit as alternate_core::Decode>::decode(&mut reader, ctx).unwrap(),
        RetargetedUnit
    );
    assert_eq!(
        <RetargetedUnit as alternate_core::Packet>::NAME,
        "minecraft:retarget"
    );
    assert_eq!(
        <RetargetedUnit as alternate_core::Packet>::STATE,
        alternate_core::State::Status
    );
    assert_eq!(
        <RetargetedUnit as alternate_core::Packet>::BOUND,
        alternate_core::Bound::Client
    );
}
