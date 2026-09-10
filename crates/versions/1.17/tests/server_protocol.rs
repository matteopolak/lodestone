use lodestone_core::{Ctx, Decode, Reader, State, encode_body};
use lodestone_data::block_states;
use lodestone_model::{
    AnimationAction, BlockFace, BlockPos, ClientAction, ClientEvent, ConnectionState, Directive,
    Hand, ItemStack, Rotation, Vec3f, VersionAdapter,
};
use lodestone_server::{ChunkColumn, ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_17::{V756ServerProtocol, V758ServerProtocol};
use lodestone_v1_17::packet_ids::handshaking;
use lodestone_v1_17::packet_ids::play as play_756;
use lodestone_v1_17::packet_ids_758::play as play_758;
use lodestone_v1_17::packets::chunk::{ChunkShape, MapChunk};
use lodestone_v1_17::packets::game::JoinGame;
use lodestone_v1_17::packets::handshake::SetProtocol;
use lodestone_world::World;

const CTX: Ctx = Ctx { version: 756 };
const CTX_758: Ctx = Ctx { version: 758 };

const OPEN_WINDOW_BODY: [u8; 19] = [
    2, 2, 16, b'{', b'"', b't', b'e', b'x', b't', b'"', b':', b'"', b'C', b'h', b'e', b's', b't', b'"', b'}',
];
const WINDOW_ITEMS_BODY: [u8; 9] = [2, 7, 2, 0, 1, 22, 3, 0, 0];
const SET_SLOT_BODY: [u8; 8] = [2, 8, 0, 5, 1, 22, 1, 0];
const CURSOR_SLOT_BODY: [u8; 8] = [0xff, 8, 0xff, 0xff, 1, 22, 1, 0];
const WINDOW_CLICK_BODY: [u8; 14] = [2, 8, 0, 0, 0, 0, 1, 0, 0, 0, 1, 22, 3, 0];
const CLOSE_WINDOW_BODY: [u8; 1] = [2];

fn bare_stack(name: &str, count: u32) -> ItemStack {
    ItemStack::new(name.parse().expect("fixture item key"), count)
}

fn adapter_event(protocol: i32, packet_id: i32, payload: &[u8]) -> ClientEvent {
    let adapter = lodestone_v1_17::adapter_for(protocol);
    let mut world = World::new();
    let directives = adapter
        .handle_packet(&mut world, ConnectionState::Play, packet_id, payload)
        .unwrap_or_else(|error| panic!("literal container packet protocol {protocol} id {packet_id} decodes: {error:?}"));
    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one event, got {directives:?}");
    };
    event.clone()
}

#[test]
fn hosted_756_and_758_container_packets_use_the_literal_stateful_shapes() {
    for &(protocol, open_id, items_id, slot_id, click_id, close_client_id, close_server_id) in &[
        (756, play_756::clientbound::OPEN_WINDOW, play_756::clientbound::WINDOW_ITEMS, play_756::clientbound::SET_SLOT, play_756::serverbound::WINDOW_CLICK, play_756::clientbound::CLOSE_WINDOW, play_756::serverbound::CLOSE_WINDOW),
        (758, play_758::clientbound::OPEN_WINDOW, play_758::clientbound::WINDOW_ITEMS, play_758::clientbound::SET_SLOT, play_758::serverbound::WINDOW_CLICK, play_758::clientbound::CLOSE_WINDOW, play_758::serverbound::CLOSE_WINDOW),
    ] {
        let protocol_host = lodestone_registry::server_protocol_for_protocol(protocol).expect("hosted protocol");
        let open = protocol_host.encode_open_screen(2, "minecraft:generic_9x3", "Chest");
        assert!(matches!(open, ServerDirective::Send { packet_id, payload } if packet_id == open_id && payload == OPEN_WINDOW_BODY));
        assert_eq!(
            adapter_event(protocol, open_id, &OPEN_WINDOW_BODY),
            ClientEvent::ScreenOpened {
                window_id: 2,
                menu_type: "minecraft:generic_9x3".parse().unwrap(),
                title: lodestone_model::Text::from_json(r#"{"text":"Chest"}"#),
            }
        );

        let items = [None, Some(bare_stack("minecraft:oak_planks", 3))];
        let content = protocol_host.encode_container_content(2, 7, &items, None);
        assert!(matches!(content, ServerDirective::Send { packet_id, payload } if packet_id == items_id && payload == WINDOW_ITEMS_BODY));
        assert_eq!(
            adapter_event(protocol, items_id, &WINDOW_ITEMS_BODY),
            ClientEvent::ContainerContent {
                window_id: 2,
                state_id: lodestone_model::ContainerStateId::from_wire(7),
                items: vec![None, Some(bare_stack("minecraft:oak_planks", 3))],
                carried_item: None,
            }
        );

        let slot = protocol_host.encode_container_slot(2, 8, 5, Some(&bare_stack("minecraft:oak_planks", 1)));
        assert!(matches!(slot, ServerDirective::Send { packet_id, payload } if packet_id == slot_id && payload == SET_SLOT_BODY));
        assert_eq!(
            adapter_event(protocol, slot_id, &SET_SLOT_BODY),
            ClientEvent::ContainerSlot {
                window_id: 2,
                state_id: lodestone_model::ContainerStateId::from_wire(8),
                slot: 5,
                item: Some(bare_stack("minecraft:oak_planks", 1)),
            }
        );
        let cursor = protocol_host.encode_container_slot(-1, 8, -1, Some(&bare_stack("minecraft:oak_planks", 1)));
        assert!(matches!(cursor, ServerDirective::Send { packet_id, payload } if packet_id == slot_id && payload == CURSOR_SLOT_BODY));
        assert_eq!(
            adapter_event(protocol, slot_id, &CURSOR_SLOT_BODY),
            ClientEvent::CursorItemChanged { item: Some(bare_stack("minecraft:oak_planks", 1)) }
        );

        let click = ClientAction::ContainerClick {
            window_id: 2,
            state_id: lodestone_model::ContainerStateId::from_wire(8),
            slot: 0,
            button: 0,
            click_type: lodestone_model::ContainerClickType::Pickup,
            changed_slots: vec![lodestone_model::ContainerSlotChange { slot: 0, item: None }],
            carried_item: Some(bare_stack("minecraft:oak_planks", 3)),
        };
        let (encoded_id, encoded_body) = lodestone_v1_17::adapter_for(protocol)
            .encode_action(ConnectionState::Play, &click)
            .expect("container click encodes")
            .expect("container click has a packet");
        assert_eq!(encoded_id, click_id);
        assert_eq!(encoded_body, WINDOW_CLICK_BODY);
        assert_eq!(
            protocol_host.decode(State::Play, click_id, &WINDOW_CLICK_BODY),
            ServerBound::ContainerClicked {
                window_id: 2,
                state_id: 8,
                slot: 0,
                button: 0,
                click_type: 0,
                changed_slots: vec![(0, None)],
                carried_item: Some(bare_stack("minecraft:oak_planks", 3)),
            }
        );

        assert_eq!(
            protocol_host.decode(State::Play, close_server_id, &CLOSE_WINDOW_BODY),
            ServerBound::ContainerClosed { window_id: 2 }
        );
        assert_eq!(
            adapter_event(protocol, close_client_id, &CLOSE_WINDOW_BODY),
            ClientEvent::ScreenClosed { window_id: 2 }
        );
    }
}

/// These are literal Play packet bodies for the four legacy movement forms.
/// The values deliberately have negative and fractional components so swapping
/// a field, treating a double as a float, or losing the final grounded flag is
/// observable without asking the encoder under test for expected bytes.
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

/// Literal one-string chat body, assembled separately from the packet codec.
/// The ASCII-only text keeps the VarInt byte length visibly equal to the
/// character count; a decoder that accepts trailing data or the wrong state
/// must still reject the controls below.
const CHAT_BODY: [u8; 14] = [
    0x0d, // string byte length
    b'e', b'r', b'a', b' ', b'c', b'h', b'a', b't', b' ', b'7', b'5', b'6', b'!',
];

fn assert_legacy_chat_lift<P: ServerProtocol>(protocol: &P, packet_id: i32) {
    let expected = ServerBound::Chat {
        message: "era chat 756!".to_owned(),
        timestamp_millis: 0,
        salt: 0,
        signature: None,
    };
    assert_eq!(
        protocol.decode(State::Play, packet_id, &CHAT_BODY),
        expected,
        "the unsigned one-string body must reach the shared chat consumer"
    );
    assert_eq!(
        protocol.decode(State::Play, packet_id, &[0x04, b'o']),
        ServerBound::Ignored,
        "a truncated string body must not reach the chat consumer"
    );
    let mut trailing = CHAT_BODY.to_vec();
    trailing.push(0);
    assert_eq!(
        protocol.decode(State::Play, packet_id, &trailing),
        ServerBound::Ignored,
        "a trailing byte must not be treated as another valid chat field"
    );
    assert_eq!(
        protocol.decode(State::Configuration, packet_id, &CHAT_BODY),
        ServerBound::Ignored,
        "chat must not bypass the direct login-to-Play state boundary"
    );
}

#[test]
fn protocol_756_lifts_literal_legacy_chat_body() {
    assert_legacy_chat_lift(
        &V756ServerProtocol,
        lodestone_v1_17::packet_ids::play::serverbound::CHAT,
    );
}

#[test]
fn protocol_758_lifts_literal_legacy_chat_body() {
    assert_legacy_chat_lift(
        &V758ServerProtocol,
        lodestone_v1_17::packet_ids_758::play::serverbound::CHAT,
    );
}

fn assert_movement_lift<P: ServerProtocol>(protocol: &P, position: i32, position_look: i32, look: i32, flying: i32) {
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
        protocol.decode(State::Play, look, &[0x40, 0x20, 0x00, 0x00, 0xc1, 0xa0, 0x00, 0x00, 0x01]),
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
fn protocol_756_lifts_literal_movement_bodies() {
    use lodestone_v1_17::packet_ids::play::serverbound;

    assert_movement_lift(
        &V756ServerProtocol,
        serverbound::POSITION,
        serverbound::POSITION_LOOK,
        serverbound::LOOK,
        serverbound::FLYING,
    );
}

#[test]
fn protocol_758_lifts_literal_movement_bodies() {
    use lodestone_v1_17::packet_ids_758::play::serverbound;

    assert_movement_lift(
        &V758ServerProtocol,
        serverbound::POSITION,
        serverbound::POSITION_LOOK,
        serverbound::LOOK,
        serverbound::FLYING,
    );
}

fn assert_block_place_lift<P: ServerProtocol>(protocol: &P, packet_id: i32) {
    // This is a literal `block_place` body, assembled independently from the
    // field layout: off hand, (5, -10, -7), south, cursor (0.25, 1.0, 0.75),
    // and inside-block true. Neither 756 nor 758 has a prediction sequence.
    let packed = ((5_i64 & 0x3ff_ffff) << 38)
        | ((-7_i64 & 0x3ff_ffff) << 12)
        | (-10_i64 & 0xfff);
    let body = [
        vec![0x01],
        packed.to_be_bytes().to_vec(),
        vec![0x03],
        0.25_f32.to_be_bytes().to_vec(),
        1.0_f32.to_be_bytes().to_vec(),
        0.75_f32.to_be_bytes().to_vec(),
        vec![0x01],
    ]
    .concat();
    assert_eq!(
        protocol.decode(State::Play, packet_id, &body),
        ServerBound::UseItemOn {
            pos: BlockPos::new(5, -10, -7),
            face: BlockFace::South,
            cursor: Vec3f {
                x: 0.25,
                y: 1.0,
                z: 0.75,
            },
            sequence: 0,
            hand: 1,
        }
    );

    let mut invalid_face = body.clone();
    invalid_face[9] = 0x06;
    assert_eq!(
        protocol.decode(State::Play, packet_id, &invalid_face),
        ServerBound::Ignored,
        "a direction outside the six block faces must not reach placement"
    );
    assert_eq!(
        protocol.decode(State::Configuration, packet_id, &body),
        ServerBound::Ignored,
        "the Play action must not bypass the connection-state gate"
    );
}

#[test]
fn protocol_756_lifts_literal_block_place_body() {
    assert_block_place_lift(
        &V756ServerProtocol,
        lodestone_v1_17::packet_ids::play::serverbound::BLOCK_PLACE,
    );
}

#[test]
fn protocol_758_lifts_literal_block_place_body() {
    assert_block_place_lift(
        &V758ServerProtocol,
        lodestone_v1_17::packet_ids_758::play::serverbound::BLOCK_PLACE,
    );
}

#[test]
fn registry_selected_1_17_era_arm_animation_reaches_the_shared_swing_consumer() {
    for (protocol_version, server_arm, client_animation) in [
        (
            756,
            lodestone_v1_17::packet_ids::play::serverbound::ARM_ANIMATION,
            lodestone_v1_17::packet_ids::play::clientbound::ANIMATION,
        ),
        (
            758,
            lodestone_v1_17::packet_ids_758::play::serverbound::ARM_ANIMATION,
            lodestone_v1_17::packet_ids_758::play::clientbound::ANIMATION,
        ),
    ] {
        let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
            .expect("every hosted 1.17-era protocol must select a server protocol");
        let adapter = lodestone_v1_17::adapter_for(protocol_version);

        for (wire_hand, expected_action) in [
            (0_u8, AnimationAction::SwingMainHand),
            (1_u8, AnimationAction::SwingOffHand),
        ] {
            // A one-byte VarInt hand body is deliberately assembled without
            // the adapter encoder, keeping the hosted decoder independently
            // pinned to the two valid wire ordinals.
            let body = [wire_hand];
            assert_eq!(
                protocol.decode(State::Play, server_arm, &body),
                ServerBound::Swing {
                    hand: Hand::from_wire_ordinal(i32::from(wire_hand)).expect("fixture hand"),
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

            let animation = if wire_hand == 0 { 0 } else { 3 };
            let ServerDirective::Send { packet_id, payload } =
                protocol.encode_animate(321, animation)
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
                    .expect("the era adapter must decode the hosted animation"),
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
fn registry_selected_1_17_era_hotbar_selection_reaches_the_inventory_consumer() {
    for (protocol_version, held_item_slot) in [
        (
            756,
            lodestone_v1_17::packet_ids::play::serverbound::HELD_ITEM_SLOT,
        ),
        (
            758,
            lodestone_v1_17::packet_ids_758::play::serverbound::HELD_ITEM_SLOT,
        ),
    ] {
        let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
            .expect("every hosted 1.17-era protocol must select a server protocol");

        // The body is a signed big-endian i16. These literals are independent
        // of the adapter encoder and pin both legal boundaries.
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

        let adapter = lodestone_v1_17::adapter_for(protocol_version);
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
fn registry_selected_1_17_era_keep_alive_round_trip_reaches_the_liveness_consumer() {
    const ID: i64 = 0x0102_0304_0506_0708;
    const BODY: [u8; 8] = ID.to_be_bytes();

    for (protocol_version, clientbound_id, serverbound_id) in [
        (
            756,
            lodestone_v1_17::packet_ids::play::clientbound::KEEP_ALIVE,
            lodestone_v1_17::packet_ids::play::serverbound::KEEP_ALIVE,
        ),
        (
            758,
            lodestone_v1_17::packet_ids_758::play::clientbound::KEEP_ALIVE,
            lodestone_v1_17::packet_ids_758::play::serverbound::KEEP_ALIVE,
        ),
    ] {
        let protocol = lodestone_registry::server_protocol_for_protocol(protocol_version)
            .expect("every hosted 1.17-era protocol must select a server protocol");
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

        let adapter = lodestone_v1_17::adapter_for(protocol_version);
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
fn protocol_756_uses_its_capture_ids_and_encodes_a_1_17_chunk() {
    let protocol = V756ServerProtocol;
    let captured_ids: Vec<i32> = include_str!("captures/join_1_17_1.txt")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == "play").then(|| fields.next()?.parse().ok())?
        })
        .collect();
    for expected in [38, 56, 34] {
        assert!(
            captured_ids.contains(&expected),
            "the committed 1.17.1 capture must contain play packet id {expected}"
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
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(756)),
        ServerBound::Handshake { next_state: State::Login }
    );
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(758)),
        ServerBound::Ignored
    );
    assert!(!protocol.has_configuration_phase());

    let play = protocol.begin_play(8);
    let ServerDirective::Send { packet_id, payload } = &play[0] else {
        panic!("begin_play must start with join");
    };
    assert_eq!(*packet_id, 38);
    let mut join_reader = Reader::new(payload);
    let join = JoinGame::decode(&mut join_reader, CTX).expect("join follows protocol-756 layout");
    join_reader.ensure_empty().expect("join has no trailing bytes");
    assert_eq!(join.view_distance, 8);
    assert_eq!(join.world_name, "minecraft:overworld");
    assert!(matches!(
        play.get(1),
        Some(ServerDirective::Send { packet_id: 56, .. })
    ));

    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(3, 100, 5, "minecraft:stone");
    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(7, -4, &column)
        .expect("stone has an exact 1.17.1 representation")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, 34);
    let mut chunk_reader = Reader::new(&payload);
    let decoded = MapChunk::decode(&mut chunk_reader, &ChunkShape::overworld(756))
        .expect("encoded chunk follows the protocol-756 layout");
    chunk_reader.ensure_empty().expect("chunk has no trailing bytes");
    assert_eq!(decoded.column.get_block(3, 100, 5), block_states::state_id("minecraft:stone").unwrap());

    assert!(matches!(
        protocol.try_encode_block_update(3, 100, 5, "minecraft:stone"),
        Ok(ServerDirective::Send { packet_id: 12, .. })
    ));
    let error = protocol
        .try_encode_block_update(3, 100, 5, "minecraft:sculk")
        .expect_err("a post-1.17 state must not substitute into an older packet");
    assert!(error.to_string().contains("protocol-756"));
}

#[test]
fn protocol_758_uses_its_capture_ids_and_encodes_an_inline_light_chunk() {
    let protocol = V758ServerProtocol;
    let captured_ids: Vec<i32> = include_str!("captures/join_1_18_2.txt")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == "play").then(|| fields.next()?.parse().ok())?
        })
        .collect();
    for expected in [38, 56, 34] {
        assert!(
            captured_ids.contains(&expected),
            "the committed 1.18.2 capture must contain play packet id {expected}"
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
            CTX_758,
        )
        .expect("handshake fixture encodes")
    };
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(758)),
        ServerBound::Handshake { next_state: State::Login }
    );
    assert_eq!(
        protocol.decode(State::Handshaking, handshaking::serverbound::SET_PROTOCOL, &request(756)),
        ServerBound::Ignored
    );

    let play = protocol.begin_play(8);
    let ServerDirective::Send { packet_id, payload } = &play[0] else {
        panic!("begin_play must start with join");
    };
    assert_eq!(*packet_id, 38);
    let mut join_reader = Reader::new(payload);
    let join = JoinGame::decode(&mut join_reader, CTX_758).expect("join follows protocol-758 layout");
    join_reader.ensure_empty().expect("join has no trailing bytes");
    assert_eq!(join.simulation_distance, 8);
    assert_eq!(join.world_name, "minecraft:overworld");

    let mut column = ChunkColumn::new(-64, 384);
    column.set_block(3, 100, 5, "minecraft:stone");
    let ServerDirective::Send { packet_id, payload } = protocol
        .try_encode_chunk(7, -4, &column)
        .expect("stone has an exact 1.18.2 representation")
    else {
        panic!("chunk encoder must produce a packet");
    };
    assert_eq!(packet_id, 34);
    let mut chunk_reader = Reader::new(&payload);
    let decoded = MapChunk::decode(&mut chunk_reader, &ChunkShape::overworld(758))
        .expect("encoded chunk follows the protocol-758 layout");
    chunk_reader.ensure_empty().expect("chunk has no trailing bytes");
    assert_eq!(decoded.column.get_block(3, 100, 5), block_states::state_id("minecraft:stone").unwrap());

    assert!(matches!(
        protocol.try_encode_block_update(3, 100, 5, "minecraft:stone"),
        Ok(ServerDirective::Send { packet_id: 12, .. })
    ));
    let error = protocol
        .try_encode_block_update(3, 100, 5, "minecraft:sculk")
        .expect_err("a post-1.18 state must not substitute into an older packet");
    assert!(error.to_string().contains("protocol-758"));
}
