use lodestone_core::{Ctx, Decode, Reader, State, encode_body, read_named_nbt};
use lodestone_data::block_states;
use lodestone_model::{
    AnimationAction, BlockFace, BlockPos, ClientAction, ClientEvent, ConnectionState, Directive,
    Hand, Rotation, Vec3f, VersionAdapter,
};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_14::{
    V498ServerProtocol, V578ServerProtocol, V754ServerProtocol, adapter_for,
};
use lodestone_v1_14::packet_ids;
use lodestone_v1_14::packet_ids_498;
use lodestone_v1_14::packet_ids_578::{handshaking, login, play};
use lodestone_v1_14::packets::chunk::{ChunkShape, MapChunk};
use lodestone_v1_14::packets::game::{JoinGame, JoinGameLegacy};
use lodestone_v1_14::packets::handshake::SetProtocol;
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 578 };
const PLAINS_BIOME_BYTES: [u8; 4] = 1_i32.to_be_bytes();

/// Literal 1.14-era Play bodies for the four ordinary movement forms. The
/// fractional and negative values prove the server decoder independently of
/// the client-side encoder.
const POSITION_BODY: [u8; 25] = [
    0xbf, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // x = -1.5
    0x40, 0x50, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, // y = 64.25
    0x40, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // z = 32.0
    0x01, // on_ground = true
];
const POSITION_LOOK_BODY: [u8; 33] = [
    0x40, 0x30, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, // x = 16.5
    0x40, 0x51, 0xa0, 0x00, 0x00, 0x00, 0x00, 0x00, // y = 70.5
    0xc0, 0x40, 0xa0, 0x00, 0x00, 0x00, 0x00, 0x00, // z = -33.25
    0x42, 0xb4, 0x00, 0x00, // yaw = 90.0
    0xc2, 0x34, 0x00, 0x00, // pitch = -45.0
    0x00, // on_ground = false
];

fn assert_movement_lift<P: ServerProtocol>(
    protocol: &P,
    position: i32,
    position_look: i32,
    look: i32,
    flying: i32,
) {
    assert_eq!(
        protocol.decode(State::Play, position, &POSITION_BODY),
        ServerBound::PlayerMoved {
            x: -1.5,
            y: 64.25,
            z: 32.0,
            rotation: None,
            on_ground: true,
        }
    );
    assert_eq!(
        protocol.decode(State::Play, position_look, &POSITION_LOOK_BODY),
        ServerBound::PlayerMoved {
            x: 16.5,
            y: 70.5,
            z: -33.25,
            rotation: Some(Rotation {
                yaw: 90.0,
                pitch: -45.0,
            }),
            on_ground: false,
        }
    );
    assert_eq!(
        protocol.decode(
            State::Play,
            look,
            &[0x40, 0x20, 0x00, 0x00, 0xc1, 0xa0, 0x00, 0x00, 0x01],
        ),
        ServerBound::PlayerRotated {
            yaw: 2.5,
            pitch: -20.0,
            on_ground: true,
        }
    );
    assert_eq!(
        protocol.decode(State::Play, flying, &[0]),
        ServerBound::PlayerStatusOnly { on_ground: false }
    );
}

#[test]
fn protocol_498_lifts_literal_movement_bodies() {
    use packet_ids_498::play::serverbound;

    assert_movement_lift(
        &V498ServerProtocol,
        serverbound::POSITION,
        serverbound::POSITION_LOOK,
        serverbound::LOOK,
        serverbound::FLYING,
    );
}

#[test]
fn protocol_578_lifts_literal_movement_bodies() {
    use lodestone_v1_14::packet_ids_578::play::serverbound;

    assert_movement_lift(
        &V578ServerProtocol,
        serverbound::POSITION,
        serverbound::POSITION_LOOK,
        serverbound::LOOK,
        serverbound::FLYING,
    );
}

#[test]
fn protocol_754_lifts_literal_movement_bodies() {
    use packet_ids::play::serverbound;

    assert_movement_lift(
        &V754ServerProtocol,
        serverbound::POSITION,
        serverbound::POSITION_LOOK,
        serverbound::LOOK,
        serverbound::FLYING,
    );
}

/// Independent 1.14+ `block_place` body: off hand, packed `(5, -10, -7)`,
/// south face, three IEEE-754 cursor coordinates and `inside_block`.  There is
/// no prediction sequence in this era; an accidental modern decoder would
/// leave a byte unread and be rejected by `decode_full`.
const BLOCK_USE_BODY: [u8; 23] = [
    0x01, // off hand
    0x00, 0x00, 0x01, 0x7f, 0xff, 0xff, 0x9f, 0xf6, // 5, -10, -7
    0x03, // south
    0x3e, 0x80, 0x00, 0x00, // 0.25
    0x3f, 0x80, 0x00, 0x00, // 1.0
    0x3f, 0x40, 0x00, 0x00, // 0.75
    0x01, // inside block
];

fn assert_block_use_lift<P: ServerProtocol>(protocol: &P, packet_id: i32) {
    assert_eq!(
        protocol.decode(State::Play, packet_id, &BLOCK_USE_BODY),
        ServerBound::UseItemOn {
            pos: BlockPos::new(5, -10, -7),
            face: BlockFace::South,
            cursor: Vec3f::new(0.25, 1.0, 0.75),
            sequence: 0,
            hand: 1,
        }
    );

    let mut invalid_face = BLOCK_USE_BODY;
    invalid_face[9] = 0x06;
    assert_eq!(
        protocol.decode(State::Play, packet_id, &invalid_face),
        ServerBound::Ignored,
        "a malformed face must not reach placement through a plausible packet body"
    );
}

#[test]
fn registry_selected_14_era_arm_animation_connects_to_the_client_event() {
    for (protocol_version, server_arm, client_animation) in [
        (
            498,
            packet_ids_498::play::serverbound::ARM_ANIMATION,
            packet_ids_498::play::clientbound::ANIMATION,
        ),
        (
            578,
            play::serverbound::ARM_ANIMATION,
            play::clientbound::ANIMATION,
        ),
        (
            754,
            packet_ids::play::serverbound::ARM_ANIMATION,
            packet_ids::play::clientbound::ANIMATION,
        ),
    ] {
        let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
            .expect("every hosted 1.14-era protocol must select a server protocol");
        let adapter = adapter_for(protocol_version);

        for (wire_hand, expected_hand, expected_action) in [
            (0_u8, Hand::Main, AnimationAction::SwingMainHand),
            (1_u8, Hand::Off, AnimationAction::SwingOffHand),
        ] {
            // The one-byte fixtures are assembled from the packet's literal
            // VarInt hand field, not produced by the adapter's encoder.
            let body = [wire_hand];
            assert_eq!(
                protocol.decode(State::Play, server_arm, &body),
                ServerBound::Swing {
                    hand: expected_hand,
                },
                "protocol {protocol_version} must lift its literal swing body"
            );

            let action = ClientAction::SwingArm {
                hand: if wire_hand == 0 { Hand::Main } else { Hand::Off },
            };
            let Some((encoded_id, encoded_body)) = adapter
                .encode_action(ConnectionState::Play, &action)
                .expect("the era adapter must encode arm swings")
            else {
                panic!("{action:?} must produce a serverbound packet");
            };
            assert_eq!(encoded_id, server_arm);
            assert_eq!(encoded_body, body);

            // The host's shared swing consumer supplies action 0 or 3. Keep
            // the clientbound body independently visible before the adapter
            // translates it into the client's event stream.
            let animation = if wire_hand == 0 { 0 } else { 3 };
            let ServerDirective::Send {
                packet_id,
                payload,
            } = protocol.encode_animate(321, animation)
            else {
                panic!("protocol {protocol_version} must encode an animation broadcast");
            };
            assert_eq!(packet_id, client_animation);
            assert_eq!(payload, vec![0xc1, 0x02, animation]);
            assert_eq!(
                adapter
                    .handle_packet(
                        &mut World::new(),
                        ConnectionState::Play,
                        packet_id,
                        &payload,
                    )
                    .expect("the era adapter must decode the animation broadcast"),
                vec![Directive::Emit(ClientEvent::EntityAnimation {
                    entity_id: 321,
                    action: expected_action,
                })]
            );
        }

        assert_eq!(
            protocol.decode(State::Play, server_arm, &[2]),
            ServerBound::Ignored,
            "protocol {protocol_version} must reject an unknown hand ordinal"
        );
        assert_eq!(
            protocol.decode(State::Play, server_arm, &[0, 0]),
            ServerBound::Ignored,
            "protocol {protocol_version} must reject a trailing byte"
        );
        assert_eq!(
            protocol.decode(State::Login, server_arm, &[0]),
            ServerBound::Ignored,
            "protocol {protocol_version} must not accept Play input during Login"
        );
    }
}

#[test]
fn registry_selected_14_era_hotbar_selection_reaches_the_inventory_consumer() {
    for (protocol_version, held_item_slot) in [
        (498, packet_ids_498::play::serverbound::HELD_ITEM_SLOT),
        (578, play::serverbound::HELD_ITEM_SLOT),
        (754, packet_ids::play::serverbound::HELD_ITEM_SLOT),
    ] {
        let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
            .expect("every hosted 1.14-era protocol must select a server protocol");

        // The body is a signed big-endian i16. These literals pin both legal
        // boundaries without asking either adapter direction for the bytes.
        assert_eq!(
            protocol.decode(State::Play, held_item_slot, &[0x00, 0x00]),
            ServerBound::CarriedItemChanged { slot: 0 },
        );
        assert_eq!(
            protocol.decode(State::Play, held_item_slot, &[0x00, 0x08]),
            ServerBound::CarriedItemChanged { slot: 8 },
        );
        for body in [&[0xff, 0xff][..], &[0x00, 0x09], &[0x00], &[0x00, 0x08, 0x00]] {
            assert_eq!(
                protocol.decode(State::Play, held_item_slot, body),
                ServerBound::Ignored,
                "protocol {protocol_version} must reject an invalid hotbar body {body:?}",
            );
        }
        assert_eq!(
            protocol.decode(State::Login, held_item_slot, &[0x00, 0x08]),
            ServerBound::Ignored,
            "hotbar selection must not bypass the Play-state boundary",
        );

        let adapter = adapter_for(protocol_version);
        let Some((packet_id, payload)) = adapter
            .encode_action(
                ConnectionState::Play,
                &ClientAction::SetCarriedItem { slot: 3 },
            )
            .expect("the client adapter must encode hotbar selection")
        else {
            panic!("hotbar selection must produce a packet");
        };
        assert_eq!(packet_id, held_item_slot);
        assert_eq!(payload, vec![0x00, 0x03]);
        assert_eq!(
            protocol.decode(State::Play, packet_id, &payload),
            ServerBound::CarriedItemChanged { slot: 3 },
            "the client action must reach the registry-selected hosted consumer",
        );
    }
}

#[test]
fn registry_selected_14_era_keep_alive_round_trip_reaches_the_liveness_consumer() {
    const ID: i64 = 0x0102_0304_0506_0708;
    const BODY: [u8; 8] = ID.to_be_bytes();

    for (protocol_version, clientbound_id, serverbound_id) in [
        (
            498,
            packet_ids_498::play::clientbound::KEEP_ALIVE,
            packet_ids_498::play::serverbound::KEEP_ALIVE,
        ),
        (
            578,
            play::clientbound::KEEP_ALIVE,
            play::serverbound::KEEP_ALIVE,
        ),
        (
            754,
            packet_ids::play::clientbound::KEEP_ALIVE,
            packet_ids::play::serverbound::KEEP_ALIVE,
        ),
    ] {
        let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
            .expect("every hosted 1.14-era protocol must select a server protocol");
        let ServerDirective::Send { packet_id, payload } = protocol.encode_keep_alive(ID) else {
            panic!("protocol {protocol_version} must emit a keep-alive challenge");
        };
        assert_eq!(packet_id, clientbound_id);
        assert_eq!(payload, BODY);
        assert_eq!(
            protocol.decode(State::Play, serverbound_id, &BODY),
            ServerBound::KeepAlive { id: ID },
        );
        assert_eq!(
            protocol.decode(State::Play, serverbound_id, &[BODY.as_slice(), &[0]].concat()),
            ServerBound::Ignored,
            "a trailing byte must not acknowledge the outstanding challenge",
        );
        assert_eq!(
            protocol.decode(State::Login, serverbound_id, &BODY),
            ServerBound::Ignored,
            "the reply must not bypass the Play-state boundary",
        );

        let adapter = adapter_for(protocol_version);
        let Some((encoded_id, encoded_body)) = adapter
            .encode_action(
                ConnectionState::Play,
                &ClientAction::KeepAliveResponse { id: ID },
            )
            .expect("the era adapter must encode keep-alive replies")
        else {
            panic!("keep-alive response must produce a packet");
        };
        assert_eq!(encoded_id, serverbound_id);
        assert_eq!(encoded_body, BODY);
        assert_eq!(
            protocol.decode(State::Play, encoded_id, &encoded_body),
            ServerBound::KeepAlive { id: ID },
            "the client response must reach the registry-selected liveness consumer",
        );
    }
}

#[test]
fn protocol_498_lifts_its_literal_block_use_body() {
    assert_block_use_lift(&V498ServerProtocol, packet_ids_498::play::serverbound::BLOCK_PLACE);
}

#[test]
fn protocol_578_lifts_its_literal_block_use_body() {
    assert_block_use_lift(&V578ServerProtocol, play::serverbound::BLOCK_PLACE);
}

#[test]
fn protocol_754_lifts_its_literal_block_use_body() {
    assert_block_use_lift(&V754ServerProtocol, packet_ids::play::serverbound::BLOCK_PLACE);
}

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
