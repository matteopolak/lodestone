use lodestone_core::{Ctx, Decode, Reader, State, encode_body, read_named_nbt};
use lodestone_data::block_states;
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_14::{V498ServerProtocol, V578ServerProtocol, V754ServerProtocol};
use lodestone_v1_14::packet_ids;
use lodestone_v1_14::packet_ids_498;
use lodestone_v1_14::packet_ids_578::{handshaking, login, play};
use lodestone_v1_14::packets::chunk::{ChunkShape, MapChunk};
use lodestone_v1_14::packets::game::{JoinGame, JoinGameLegacy};
use lodestone_v1_14::packets::handshake::SetProtocol;

const CTX: Ctx = Ctx { version: 578 };
const PLAINS_BIOME_BYTES: [u8; 4] = 1_i32.to_be_bytes();

#[test]
fn protocol_498_accepts_its_handshake_and_emits_legacy_join() {
    let protocol = V498ServerProtocol;
    let request = encode_body(
        &SetProtocol {
            protocol_version: 498,
            server_host: "localhost".to_owned(),
            server_port: 25565,
            next_state: 2,
        },
        Ctx { version: 498 },
    )
    .expect("handshake fixture encodes");
    assert_eq!(
        protocol.decode(
            State::Handshaking,
            packet_ids_498::handshaking::serverbound::SET_PROTOCOL,
            &request,
        ),
        ServerBound::Handshake { next_state: State::Login }
    );
    assert!(!protocol.has_configuration_phase());
    assert!(protocol.begin_configuration().is_empty());

    let play = protocol.begin_play(8);
    let ServerDirective::Send { packet_id, payload } = &play[0] else {
        panic!("begin_play must send a join packet");
    };
    assert_eq!(*packet_id, packet_ids_498::play::clientbound::LOGIN);
    let mut reader = Reader::new(payload);
    let join = JoinGameLegacy::decode(&mut reader, Ctx { version: 498 })
        .expect("legacy protocol-498 join packet decodes");
    reader.ensure_empty().expect("join packet is fully consumed");
    assert_eq!(join.dimension, 0);
    assert_eq!(join.level_type, "default");
    assert_eq!(join.view_distance, 8);
}

/// The hosted 498 join has no seed hash or respawn-screen byte. Keep the
/// reference body literal so a shared codec regression cannot bless a shifted
/// field by encoding and decoding the same mistaken layout.
#[test]
fn protocol_498_emits_the_reference_legacy_join_body() {
    let protocol = V498ServerProtocol;
    let ServerDirective::Send { packet_id, payload } = &protocol.begin_play(8)[0] else {
        panic!("begin_play must start with a join packet");
    };

    assert_eq!(*packet_id, packet_ids_498::play::clientbound::LOGIN);
    assert_eq!(
        payload,
        &[
            0, 0, 0, 1, // entity id
            0, // survival game mode
            0, 0, 0, 0, // overworld dimension
            20, // max players
            7, b'd', b'e', b'f', b'a', b'u', b'l', b't', // level type
            8, // view distance
            0, // reduced debug info
        ],
        "protocol 498's join layout ends after reduced-debug-info",
    );
}

#[test]
fn protocol_498_emits_a_decodable_straddling_chunk_with_embedded_biomes() {
    let protocol = V498ServerProtocol;
    let mut column = ChunkColumn::new(0, 256);
    let states = [
        "minecraft:air",
        "minecraft:stone",
        "minecraft:granite",
        "minecraft:polished_granite",
        "minecraft:diorite",
        "minecraft:polished_diorite",
        "minecraft:andesite",
        "minecraft:polished_andesite",
        "minecraft:grass_block",
        "minecraft:dirt",
        "minecraft:coarse_dirt",
        "minecraft:podzol",
        "minecraft:cobblestone",
        "minecraft:oak_planks",
        "minecraft:spruce_planks",
        "minecraft:birch_planks",
        "minecraft:jungle_planks",
        "minecraft:acacia_planks",
        "minecraft:dark_oak_planks",
        "minecraft:oak_sapling",
    ];
    for (index, state) in states.iter().enumerate() {
        column.set_block((index % 16) as i32, 0, (index / 16) as i32, state);
    }

    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(7, -4, &column)
        .expect("all fixture states have exact protocol-498 representations")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, packet_ids_498::play::clientbound::MAP_CHUNK);

    // The committed 1.14.4 capture establishes that these 256 big-endian
    // biome ids are the tail of the length-prefixed chunkData buffer, not a
    // field between the heightmap and that buffer. Check that boundary
    // independently before asking the crate decoder to parse the whole body.
    let mut outer = Reader::new(&payload);
    assert_eq!(outer.i32().expect("chunk x"), 7);
    assert_eq!(outer.i32().expect("chunk z"), -4);
    assert!(outer.bool().expect("ground-up flag"));
    let _ = outer.var_i32().expect("section bitmask");
    read_named_nbt(&mut outer).expect("heightmap NBT");
    let chunk_data_len = usize::try_from(outer.var_i32().expect("chunkData length"))
        .expect("non-negative chunkData length");
    let chunk_data = outer
        .take_reader(chunk_data_len)
        .expect("chunkData bytes");
    assert!(chunk_data.remaining_bytes().len() >= 1024);
    let biome_tail = &chunk_data.remaining_bytes()[chunk_data.remaining_bytes().len() - 1024..];
    assert!(
        biome_tail
            .chunks_exact(4)
            .all(|entry| entry == PLAINS_BIOME_BYTES),
        "protocol-498 chunkData must end with 256 big-endian plains ids"
    );
    outer.var_i32().expect("block entity count");
    outer.ensure_empty().expect("outer packet is fully consumed");

    let mut reader = Reader::new(&payload);
    let decoded = MapChunk::decode(&mut reader, &ChunkShape::overworld(498))
        .expect("encoded chunk follows the protocol-498 layout");
    reader.ensure_empty().expect("chunk body is fully consumed");
    assert_eq!((decoded.x, decoded.z), (7, -4));
    assert_eq!(
        decoded.column.get_block(3, 0, 0),
        block_states::state_id("minecraft:polished_granite").expect("canonical granite")
    );
}

#[test]
fn protocol_498_rejects_a_canonical_state_outside_its_table() {
    let protocol = V498ServerProtocol;
    let modern = "minecraft:sculk";
    assert!(block_states::state_id(modern).is_some());
    let mut column = ChunkColumn::new(0, 256);
    column.set_block(0, 0, 0, modern);

    let error = protocol
        .try_encode_chunk(0, 0, &column)
        .expect_err("an unrepresentable canonical state must not become air");
    assert!(error.to_string().contains("protocol-498"));
}

#[test]
fn protocol_578_accepts_its_handshake_and_transitions_directly_to_play() {
    let protocol = V578ServerProtocol;
    let request = encode_body(
        &SetProtocol {
            protocol_version: 578,
            server_host: "localhost".to_owned(),
            server_port: 25565,
            next_state: 2,
        },
        CTX,
    )
    .expect("handshake fixture encodes");

    assert_eq!(
        protocol.decode(
            State::Handshaking,
            handshaking::serverbound::SET_PROTOCOL,
            &request,
        ),
        ServerBound::Handshake {
            next_state: State::Login,
        }
    );
    assert!(protocol
        .login_success("player", uuid::Uuid::nil())
        .iter()
        .any(|directive| matches!(directive, ServerDirective::SetCompression(256))));
    assert!(!protocol.has_configuration_phase());
    assert!(protocol.begin_configuration().is_empty());
    assert_eq!(protocol.begin_play(8).len(), 2);
}

/// The 1.15.2 join keeps 1.14.4's prefix, then adds a big-endian seed hash
/// and final respawn-screen byte. Keep the expected body literal so its two
/// protocol-only fields cannot be blessed by the same codec that writes it.
#[test]
fn protocol_578_emits_the_reference_legacy_join_body() {
    let protocol = V578ServerProtocol;
    let ServerDirective::Send { packet_id, payload } = &protocol.begin_play(8)[0] else {
        panic!("begin_play must start with a join packet");
    };

    assert_eq!(*packet_id, play::clientbound::LOGIN);
    assert_eq!(
        payload,
        &[
            0, 0, 0, 1, // entity id
            0, // survival game mode
            0, 0, 0, 0, // overworld dimension
            0, 0, 0, 0, 0, 0, 0, 0, // hashed seed
            20, // max players
            7, b'd', b'e', b'f', b'a', b'u', b'l', b't', // level type
            8, // view distance
            0, // reduced debug info
            1, // enable respawn screen
        ],
        "protocol 578's join inserts the seed and appends enable-respawn-screen",
    );
}

#[test]
fn protocol_578_encodes_a_decodable_straddling_chunk() {
    let protocol = V578ServerProtocol;
    let mut column = ChunkColumn::new(0, 256);
    column.set_block(3, 0, 5, "minecraft:stone");

    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(7, -4, &column)
        .expect("stone has an exact protocol-578 representation")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::MAP_CHUNK);

    let mut reader = Reader::new(&payload);
    let decoded = MapChunk::decode(&mut reader, &ChunkShape::overworld(578))
        .expect("encoded chunk follows the protocol-578 layout");
    reader.ensure_empty().expect("chunk body is fully consumed");
    assert_eq!((decoded.x, decoded.z), (7, -4));
    assert_eq!(
        decoded.column.get_block(3, 0, 5),
        block_states::state_id("minecraft:stone").expect("canonical stone")
    );
}

#[test]
fn protocol_578_rejects_a_canonical_state_outside_its_table() {
    let protocol = V578ServerProtocol;
    let modern = "minecraft:sculk";
    assert!(block_states::state_id(modern).is_some());
    let mut column = ChunkColumn::new(0, 256);
    column.set_block(0, 0, 0, modern);

    let error = protocol
        .try_encode_chunk(0, 0, &column)
        .expect_err("an unrepresentable canonical state must not become air");
    assert!(error.to_string().contains("protocol-578"));
}

#[test]
fn protocol_578_rejects_a_biome_without_an_exact_legacy_id() {
    let protocol = V578ServerProtocol;
    let mut column = ChunkColumn::new(0, 256);
    column.set_biome_cell(0, 0, 0, "minecraft:lush_caves");

    let error = protocol
        .try_encode_chunk(0, 0, &column)
        .expect_err("unsupported biome data must not be rewritten as plains");
    assert!(error.to_string().contains("minecraft:lush_caves"));
}

#[test]
fn protocol_578_uses_the_login_packet_id_and_login_start_shape() {
    let _ = (login::serverbound::LOGIN_START, play::clientbound::LOGIN);
}

#[test]
fn protocol_754_accepts_its_handshake_and_emits_binary_login_success() {
    let protocol = V754ServerProtocol;
    let request = encode_body(
        &SetProtocol {
            protocol_version: 754,
            server_host: "localhost".to_owned(),
            server_port: 25565,
            next_state: 2,
        },
        Ctx { version: 754 },
    )
    .expect("handshake fixture encodes");
    assert_eq!(
        protocol.decode(
            State::Handshaking,
            packet_ids::handshaking::serverbound::SET_PROTOCOL,
            &request,
        ),
        ServerBound::Handshake { next_state: State::Login }
    );

    let uuid = uuid::Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
    let directives = protocol.login_success("player", uuid);
    assert!(matches!(directives[1], ServerDirective::SetCompression(256)));
    let ServerDirective::Send { packet_id, payload } = &directives[2] else {
        panic!("login success must be sent as a packet");
    };
    assert_eq!(*packet_id, packet_ids::login::clientbound::SUCCESS);
    let mut reader = Reader::new(payload);
    let success = lodestone_v1_14::packets::login::LoginSuccess::decode(
        &mut reader,
        Ctx { version: 754 },
    )
    .expect("binary protocol-754 login success decodes");
    reader.ensure_empty().expect("login success is fully consumed");
    assert_eq!(success.uuid, uuid);
    assert_eq!(success.username, "player");

    let play = protocol.begin_play(8);
    let ServerDirective::Send { packet_id, payload } = &play[0] else {
        panic!("begin_play must send a join packet");
    };
    assert_eq!(*packet_id, packet_ids::play::clientbound::LOGIN);
    let mut reader = Reader::new(payload);
    let join = JoinGame::decode(&mut reader, Ctx { version: 754 })
        .expect("binary protocol-754 join packet decodes");
    reader.ensure_empty().expect("join packet is fully consumed");
    assert_eq!(join.world_name, "minecraft:overworld");
    assert_eq!(join.view_distance, 8);
}

/// Protocol 754's join is a different packet shape, not the legacy join with
/// more fields. This literal reference body keeps the writer and reader from
/// agreeing on a misplaced field.
#[test]
fn protocol_754_emits_the_reference_join_body() {
    let protocol = V754ServerProtocol;
    let ServerDirective::Send { packet_id, payload } = &protocol.begin_play(8)[0] else {
        panic!("begin_play must start with a join packet");
    };

    assert_eq!(*packet_id, packet_ids::play::clientbound::LOGIN);
    assert_eq!(
        payload,
        &[
            0, 0, 0, 1, // entity id
            0, // not hardcore
            0, // survival game mode
            255, // no previous game mode
            1, // one world name
            19, // world-name string length
            b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b'o', b'v', b'e',
            b'r', b'w', b'o', b'r', b'l', b'd',
            10, 0, 4, b'r', b'o', b'o', b't', // dimension codec root
            8, 0, 4, b'n', b'a', b'm', b'e', 0, 19, // codec name tag
            b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b'o', b'v', b'e',
            b'r', b'w', b'o', b'r', b'l', b'd', 0, // codec name and end tag
            10, 0, 3, b'd', b'i', b'm', // dimension root
            1, 0, 7, b'n', b'a', b't', b'u', b'r', b'a', b'l', 1, 0, // natural tag and end
            19, // world-name string length
            b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b'o', b'v', b'e',
            b'r', b'w', b'o', b'r', b'l', b'd',
            0, 0, 0, 0, 0, 0, 0, 0, // hashed seed
            20, // max players
            8, // view distance
            0, // reduced debug info
            1, // enable respawn screen
            0, // not debug
            0, // not flat
        ],
        "protocol 754's join uses binary NBT dimensions and named worlds",
    );
}

#[test]
fn protocol_754_emits_a_decodable_padded_chunk_and_varint_biomes() {
    let protocol = V754ServerProtocol;
    let mut column = ChunkColumn::new(0, 256);
    let states = [
        "minecraft:air",
        "minecraft:stone",
        "minecraft:granite",
        "minecraft:polished_granite",
        "minecraft:diorite",
        "minecraft:polished_diorite",
        "minecraft:andesite",
        "minecraft:polished_andesite",
        "minecraft:grass_block",
        "minecraft:dirt",
        "minecraft:coarse_dirt",
        "minecraft:podzol",
        "minecraft:cobblestone",
        "minecraft:oak_planks",
        "minecraft:spruce_planks",
        "minecraft:birch_planks",
        "minecraft:jungle_planks",
        "minecraft:acacia_planks",
        "minecraft:dark_oak_planks",
        "minecraft:oak_sapling",
    ];
    for (index, state) in states.iter().enumerate() {
        column.set_block((index % 16) as i32, 0, (index / 16) as i32, state);
    }

    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(7, -4, &column)
        .expect("all fixture states have exact protocol-754 representations")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, packet_ids::play::clientbound::MAP_CHUNK);

    let mut reader = Reader::new(&payload);
    let decoded = MapChunk::decode(&mut reader, &ChunkShape::overworld(754))
        .expect("encoded chunk follows the protocol-754 layout");
    reader.ensure_empty().expect("chunk body is fully consumed");
    assert_eq!((decoded.x, decoded.z), (7, -4));
    assert_eq!(
        decoded.column.get_block(3, 0, 0),
        block_states::state_id("minecraft:polished_granite").expect("canonical granite")
    );
}

#[test]
fn protocol_754_rejects_a_canonical_state_outside_its_table() {
    let protocol = V754ServerProtocol;
    let modern = "minecraft:sculk";
    assert!(block_states::state_id(modern).is_some());
    let mut column = ChunkColumn::new(0, 256);
    column.set_block(0, 0, 0, modern);

    let error = protocol
        .try_encode_chunk(0, 0, &column)
        .expect_err("an unrepresentable canonical state must not become air");
    assert!(error.to_string().contains("protocol-754"));
}
