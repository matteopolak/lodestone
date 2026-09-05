use lodestone_core::{Ctx, Decode, Reader, State, encode_body};
use lodestone_data::block_states;
use lodestone_model::{
    AnimationAction, BlockActionKind, BlockFace, BlockPos, ClientAction, ClientEvent,
    ConnectionState, Directive, Hand, Rotation, Vec3f, VersionAdapter,
};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_19::{V762Adapter, V762ServerProtocol};
use lodestone_v1_19::packet_ids::{handshaking, play};
use lodestone_v1_19::packets::chunk::{ChunkShape, MapChunk};
use lodestone_v1_19::packets::game::{BlockDig, JoinGame};
use lodestone_v1_19::packets::handshake::SetProtocol;
use lodestone_v1_19::packets::position::Position;
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 762 };

#[test]
fn protocol_762_uses_its_capture_ids_and_encodes_a_registry_shaped_chunk() {
    let protocol = V762ServerProtocol;
    let captured_ids: Vec<i32> = include_str!("captures/join_1_19_4.txt")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == "play").then(|| fields.next()?.parse().ok())?
        })
        .collect();
    for expected in [36, 60] {
        assert!(
            captured_ids.contains(&expected),
            "the committed 1.19.4 capture must contain play packet id {expected}"
        );
    }

    let request = |protocol_version| {
        encode_body(
            &SetProtocol {
                protocol_version,
                server_host: "localhost".to_owned(),
                server_port: 25565,
                next_state: 2,
            },
            CTX,
        )
        .expect("handshake fixture encodes")
    };
    assert_eq!(
        protocol.decode(
            State::Handshaking,
            handshaking::serverbound::SET_PROTOCOL,
            &request(762)
        ),
        ServerBound::Handshake {
            next_state: State::Login
        }
    );
    assert_eq!(
        protocol.decode(
            State::Handshaking,
            handshaking::serverbound::SET_PROTOCOL,
            &request(761)
        ),
        ServerBound::Ignored
    );
    assert!(!protocol.has_configuration_phase());

    let play_directives = protocol.begin_play(8);
    let ServerDirective::Send { packet_id, payload } = &play_directives[0] else {
        panic!("begin_play must start with join");
    };
    assert_eq!(*packet_id, play::clientbound::LOGIN);
    let mut join_reader = Reader::new(payload);
    let join = JoinGame::decode(&mut join_reader, CTX).expect("join follows protocol-762 layout");
    join_reader.ensure_empty().expect("join has no trailing bytes");
    assert_eq!(join.view_distance, 8);
    assert_eq!(join.simulation_distance, 8);
    assert_eq!(join.world_type, "minecraft:overworld");
    assert_eq!(join.world_name, "minecraft:overworld");
    let shape = ChunkShape::overworld(762)
        .from_dimension_registry(&join.dimension_codec, &join.world_type)
        .expect("hosted join declares the selected dimension window");
    assert_eq!((shape.min_y, shape.section_count), (-64, 24));
    assert!(matches!(
        play_directives.get(1),
        Some(ServerDirective::Send { packet_id, .. }) if *packet_id == play::clientbound::POSITION
    ));

    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(3, 100, 5, "minecraft:stone");
    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(7, -4, &column)
        .expect("stone has an exact 1.19.4 representation")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::MAP_CHUNK);
    let mut chunk_reader = Reader::new(&payload);
    let decoded = MapChunk::decode(&mut chunk_reader, &ChunkShape::overworld(762))
        .expect("encoded chunk follows the protocol-762 layout");
    chunk_reader.ensure_empty().expect("chunk has no trailing bytes");
    assert_eq!(
        decoded.column.get_block(3, 100, 5),
        block_states::state_id("minecraft:stone").unwrap()
    );

    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_block_update(3, 100, 5, "minecraft:stone")
        .expect("stone block update has an exact 1.19.4 representation")
    else {
        panic!("block update encoder must produce a packet");
    };
    assert_eq!(packet_id, play::clientbound::BLOCK_CHANGE);
    let mut update_reader = Reader::new(&payload);
    assert_eq!(
        update_reader.i64().expect("block position"),
        lodestone_v1_19::packets::position::pack_position(BlockPos::new(3, 100, 5))
    );
    assert!(update_reader.var_i32().expect("wire state") >= 0);
    update_reader
        .ensure_empty()
        .expect("block update has no trailing bytes");

    let dig = encode_body(
        &BlockDig {
            status: 0,
            location: Position::new(3, 100, 5),
            face: 1,
            sequence: 17,
        },
        CTX,
    )
    .expect("block-dig fixture encodes");
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::BLOCK_DIG, &dig),
        ServerBound::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: BlockPos::new(3, 100, 5),
            face: BlockFace::Up,
            sequence: 17,
        }
    );
    let error = protocol
        .try_encode_block_update(3, 100, 5, "minecraft:creaking_heart")
        .expect_err("a newer state must not substitute into a protocol-762 packet");
    assert!(error.to_string().contains("protocol-762"));
}

#[test]
fn protocol_762_lifts_all_four_movement_shapes_from_literal_bodies() {
    let protocol = V762ServerProtocol;

    // Captured-width wire bodies, written independently of the packet codecs:
    // f64 x/y/z, then (where present) f32 yaw/pitch, then on-ground.
    let position = [
        0x40, 0x31, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // 17.25
        0x40, 0x51, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, // 70.0
        0xc0, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // -2.5
        0x01,
    ];
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::POSITION, &position),
        ServerBound::PlayerMoved {
            x: 17.25,
            y: 70.0,
            z: -2.5,
            rotation: None,
            on_ground: true,
        }
    );

    let position_look = [
        0x40, 0x40, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, // 33.5
        0x40, 0x51, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 68.0
        0xc0, 0x31, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // -17.25
        0x42, 0xb4, 0x00, 0x00, // 90.0
        0xc1, 0x48, 0x00, 0x00, // -12.5
        0x00,
    ];
    assert_eq!(
        protocol.decode(
            State::Play,
            play::serverbound::POSITION_LOOK,
            &position_look
        ),
        ServerBound::PlayerMoved {
            x: 33.5,
            y: 68.0,
            z: -17.25,
            rotation: Some(Rotation::new(90.0, -12.5)),
            on_ground: false,
        }
    );

    let look = [
        0xc2, 0xb4, 0x00, 0x00, // -90.0
        0x41, 0x70, 0x00, 0x00, // 15.0
        0x01,
    ];
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::LOOK, &look),
        ServerBound::PlayerRotated {
            yaw: -90.0,
            pitch: 15.0,
            on_ground: true,
        }
    );
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::FLYING, &[0x00]),
        ServerBound::PlayerStatusOnly { on_ground: false }
    );

    let mut trailing = position;
    let mut malformed = trailing.to_vec();
    malformed.push(0x00);
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::POSITION, &malformed),
        ServerBound::Ignored,
        "a position decoder must not accept a body that shifts the next frame"
    );
    trailing[0] = 0;
    assert_eq!(
        protocol.decode(State::Play, -1, &trailing),
        ServerBound::Ignored,
        "an unknown packet id must not turn plausible movement bytes into a move"
    );
}

#[test]
fn protocol_762_lifts_literal_block_use_with_its_prediction_sequence() {
    let protocol = V762ServerProtocol;

    // Independent 762 packet body: main/off hand VarInt, packed 5/-10/-7
    // position, south face, three IEEE-754 cursor coordinates, inside-block,
    // then the 1.19 block-prediction sequence. These bytes do not use this
    // crate's position packer or packet encoder.
    let body = [
        0x01, // off hand
        0x00, 0x00, 0x01, 0x7f, 0xff, 0xff, 0x9f, 0xf6, // 5, -10, -7
        0x03, // south
        0x3e, 0x80, 0x00, 0x00, // 0.25
        0x3f, 0x80, 0x00, 0x00, // 1.0
        0x3f, 0x40, 0x00, 0x00, // 0.75
        0x01, // inside block
        0x11, // prediction sequence 17
    ];
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::BLOCK_PLACE, &body),
        ServerBound::UseItemOn {
            pos: BlockPos::new(5, -10, -7),
            face: BlockFace::South,
            cursor: Vec3f {
                x: 0.25,
                y: 1.0,
                z: 0.75,
            },
            sequence: 17,
            hand: 1,
        }
    );

    let mut invalid_face = body;
    invalid_face[9] = 0x06;
    assert_eq!(
        protocol.decode(
            State::Play,
            play::serverbound::BLOCK_PLACE,
            &invalid_face
        ),
        ServerBound::Ignored,
        "a malformed face must not reach placement through a plausible packet body"
    );
}

#[test]
fn registry_selected_protocol_762_carries_arm_swings_to_the_client_event_stream() {
    let protocol = lodestone_registry::server_protocol_for_protocol(762)
        .expect("protocol 762 must resolve to its hosted server protocol");
    let adapter = V762Adapter::new();

    for (hand, body, expected_action) in [
        (Hand::Main, &[0][..], AnimationAction::SwingMainHand),
        (Hand::Off, &[1][..], AnimationAction::SwingOffHand),
    ] {
        // The hand VarInt is a literal capture-shaped request body. It keeps
        // the hosted decoder independent of the adapter encoder below.
        assert_eq!(
            protocol.decode(State::Play, play::serverbound::ARM_ANIMATION, body),
            ServerBound::Swing { hand },
            "the literal {hand:?} request must reach the shared swing consumer"
        );

        let action = ClientAction::SwingArm { hand };
        let Some((packet_id, payload)) = adapter
            .encode_action(ConnectionState::Play, &action)
            .expect("the protocol-762 adapter encodes arm swings")
        else {
            panic!("{action:?} must produce a protocol-762 packet");
        };
        assert_eq!(packet_id, play::serverbound::ARM_ANIMATION);
        assert_eq!(payload, body);
        assert_eq!(
            protocol.decode(State::Play, packet_id, &payload),
            ServerBound::Swing { hand },
            "the adapter request must reach the registry-selected host consumer"
        );

        let animation = if hand == Hand::Main { 0 } else { 3 };
        let ServerDirective::Send { packet_id, payload } = protocol.encode_animate(321, animation)
        else {
            panic!("the protocol-762 host must encode an animation broadcast");
        };
        assert_eq!(packet_id, play::clientbound::ANIMATION);
        assert_eq!(payload, vec![0xc1, 0x02, animation]);
        let mut world = World::new();
        assert_eq!(
            adapter
                .handle_packet(&mut world, ConnectionState::Play, packet_id, &payload)
                .expect("the protocol-762 client decodes the hosted animation"),
            vec![Directive::Emit(ClientEvent::EntityAnimation {
                entity_id: 321,
                action: expected_action,
            })]
        );
    }

    assert_eq!(
        protocol.decode(State::Play, play::serverbound::ARM_ANIMATION, &[2]),
        ServerBound::Ignored,
        "a third hand ordinal must not become an arbitrary animation"
    );
    assert_eq!(
        protocol.decode(State::Play, play::serverbound::ARM_ANIMATION, &[0, 0]),
        ServerBound::Ignored,
        "a valid hand prefix with a trailing byte must be rejected"
    );
    assert_eq!(
        protocol.decode(State::Login, play::serverbound::ARM_ANIMATION, &[0]),
        ServerBound::Ignored,
        "the Play request must not be accepted before Play"
    );
}

#[test]
fn hosted_held_slot_reaches_the_registry_selected_inventory_consumer() {
    let protocol = V762ServerProtocol;

    // The protocol-762 body is a single signed big-endian i16. This literal
    // final hotbar value does not use either direction's packet encoder.
    assert_eq!(
        protocol.decode(
            State::Play,
            play::serverbound::HELD_ITEM_SLOT,
            &[0x00, 0x08],
        ),
        ServerBound::CarriedItemChanged { slot: 8 },
        "the last legal hotbar slot must reach the shared inventory consumer"
    );
    for (body, description) in [
        (&[0xff, 0xff][..], "negative slots"),
        (&[0x00, 0x09][..], "slots beyond the hotbar"),
        (&[0x00][..], "truncated bodies"),
        (&[0x00, 0x08, 0x00][..], "trailing bytes"),
    ] {
        assert_eq!(
            protocol.decode(State::Play, play::serverbound::HELD_ITEM_SLOT, body),
            ServerBound::Ignored,
            "{description} must not change the selected inventory slot"
        );
    }
    assert_eq!(
        protocol.decode(
            State::Login,
            play::serverbound::HELD_ITEM_SLOT,
            &[0x00, 0x08],
        ),
        ServerBound::Ignored,
        "the hotbar selector belongs to Play"
    );

    let adapter = V762Adapter::new();
    let action = ClientAction::SetCarriedItem { slot: 3 };
    let Some((packet_id, payload)) = adapter
        .encode_action(ConnectionState::Play, &action)
        .expect("the protocol-762 adapter encodes hotbar selection")
    else {
        panic!("hotbar selection must produce a Play packet");
    };
    assert_eq!(packet_id, play::serverbound::HELD_ITEM_SLOT);
    assert_eq!(payload, vec![0x00, 0x03]);
    let registry_protocol = lodestone_registry::server_protocol_for_protocol(762)
        .expect("protocol 762 must resolve to its hosted server protocol");
    assert_eq!(
        registry_protocol.decode(State::Play, packet_id, &payload),
        ServerBound::CarriedItemChanged { slot: 3 },
        "the adapter action must pass through the registry-selected host to the shared inventory consumer"
    );
}
